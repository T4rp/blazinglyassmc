use reqwest;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Semaphore;

const MINECRAFT_1_20_4_META_URL: &str = "https://piston-meta.mojang.com/v1/packages/efcc510e525cef0e859b5435f82b6e3193214efc/1.20.4.json";

struct AssetIndexDownload<'a> {
    id: &'a str,
    url: &'a str,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct LauncherConfig {
    username: String,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            username: String::from("Username"),
        }
    }
}

fn _list_files(paths: &mut Vec<PathBuf>, path: &Path) {
    let dir_paths = fs::read_dir(path).unwrap();
    for entry in dir_paths {
        let entry_path = entry.unwrap().path();
        if entry_path.is_dir() {
            _list_files(paths, &entry_path);
        } else {
            paths.push(entry_path)
        }
    }
}

fn list_files(path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    _list_files(&mut paths, path);
    paths
}

async fn download_assets(
    http_client: reqwest::Client,
    assets_directory: &Path,
    asset_index_download: AssetIndexDownload<'_>,
) {
    let indexes_path = assets_directory.join("indexes");
    fs::create_dir_all(&indexes_path).unwrap();

    let asset_index_json: serde_json::Value = http_client
        .get(asset_index_download.url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let index_path = indexes_path.join(format!("{}.json", asset_index_download.id));
    fs::write(index_path, asset_index_json.clone().to_string()).unwrap();

    let objects_path = assets_directory.join("objects");
    fs::create_dir_all(&objects_path).unwrap();

    let asset_objects = asset_index_json["objects"].as_object().unwrap();

    let semaphore = Arc::new(Semaphore::new(5));

    let mut handles = Vec::new();

    for (_k, v) in asset_objects.iter() {
        let hash = v["hash"].as_str().unwrap().to_owned();
        let hash_prefix = hash[0..2].to_owned();

        let asset_parent = objects_path.join(&hash_prefix);
        let asset_path = asset_parent.join(&hash);
        fs::create_dir_all(asset_parent).unwrap();

        if !asset_path.exists() {
            let semaphore = semaphore.clone();
            let http_client = http_client.clone();

            handles.push(tokio::spawn(async move {
                let permit = semaphore.acquire().await.unwrap();

                let data = http_client
                    .get(format!(
                        "https://resources.download.minecraft.net/{}/{}",
                        hash_prefix, hash
                    ))
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap();

                fs::write(asset_path, data).unwrap();

                drop(permit);
                println!("downloaded asset {}", hash);
            }));
        }
    }

    futures::future::join_all(handles).await;
}

async fn download_libraries(
    http_client: reqwest::Client,
    libraries_directory: &Path,
    library_entries: &[serde_json::Value],
) {
    for library_entry in library_entries.iter() {
        let path = library_entry["downloads"]["artifact"]["path"]
            .as_str()
            .unwrap();

        let rules = library_entry.get("rules").map_or(None, |a| a.get(0));

        if rules.is_some() && rules.unwrap()["os"]["name"] != "windows" {
            continue;
        }

        let lib_path = libraries_directory.join(path);

        if lib_path.exists() {
            continue;
        }

        let url = library_entry["downloads"]["artifact"]["url"]
            .as_str()
            .unwrap();

        fs::create_dir_all(lib_path.parent().unwrap()).unwrap();

        println!("downloading {}", path);

        let data = http_client
            .get(url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();

        fs::write(lib_path, data).unwrap();
    }
}

async fn download_client(
    http_client: reqwest::Client,
    instance_directory: &Path,
    client_jar_url: &str,
) {
    let client_jar = instance_directory.join("client.jar");

    if client_jar.exists() {
        return;
    }

    let data = http_client
        .get(client_jar_url)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();

    fs::write(client_jar, data).unwrap()
}

async fn get_minecraft_meta(current_directory: &Path) -> serde_json::Value {
    let minecraft_meta_path = current_directory.join("1.20.4.json");

    let meta: serde_json::Value = if minecraft_meta_path.exists() {
        fs::read_to_string(&minecraft_meta_path)
            .unwrap()
            .parse::<serde_json::Value>()
            .unwrap()
    } else {
        let json: serde_json::Value = reqwest::get(MINECRAFT_1_20_4_META_URL)
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        fs::write(&minecraft_meta_path, json.to_string()).unwrap();

        json
    };

    meta
}

async fn create_config(instance_directory: &Path) {
    let config_path = instance_directory.join("LauncherConfig.toml");

    if !config_path.exists() {
        let config_str = toml::to_string(&LauncherConfig::default()).unwrap();
        fs::write(config_path, config_str).unwrap();
    }
}

async fn create_profile() {
    let current_directory = env::current_dir().unwrap();
    let minecraft_meta = get_minecraft_meta(&current_directory).await;

    let instance_directory = Path::new("instance");

    fs::create_dir_all(instance_directory).unwrap();

    let assets_directory = instance_directory.join("assets");
    let libraries_directory = instance_directory.join("libraries");

    let client_jar_url = minecraft_meta["downloads"]["client"]["url"]
        .as_str()
        .unwrap();
    let library_entries = minecraft_meta["libraries"].as_array().unwrap();
    let assets_url = minecraft_meta["assetIndex"]["url"].as_str().unwrap();

    create_config(&instance_directory).await;

    let http_client = reqwest::Client::new();

    download_client(http_client.clone(), &instance_directory, client_jar_url).await;
    download_libraries(http_client.clone(), &libraries_directory, &library_entries).await;
    download_assets(
        http_client.clone(),
        &assets_directory,
        AssetIndexDownload {
            id: "12",
            url: assets_url,
        },
    )
    .await;

    let current_exe = env::current_exe().unwrap();
    fs::copy(current_exe, instance_directory.join("start.exe")).unwrap();
}

fn launch_minecraft() {
    let parent_dir = env::current_exe().unwrap().parent().unwrap().to_owned();

    let config_path = parent_dir.join("LauncherConfig.toml");
    let client_path = parent_dir.join("client.jar");
    let libraries_path = parent_dir.join("libraries");
    let assets_path = parent_dir.join("assets");

    let config = fs::read_to_string(&config_path).unwrap();
    let config: LauncherConfig = toml::from_str(&config).unwrap();

    let mut library_file_listing = list_files(&libraries_path);
    library_file_listing.push(client_path);

    let java_libraries = library_file_listing
        .iter()
        .map(|a| a.canonicalize().unwrap().to_str().unwrap()[4..].to_owned())
        .collect::<Vec<String>>()
        .join(";");

    Command::new("javaw")
        .stdin(process::Stdio::piped())
        .stdout(process::Stdio::piped())
        .arg("-XX:HeapDumpPath=MojangTricksIntelDriversForPerformance_javaw.exe_minecraft.exe.heapdump")
        .arg("-Djava.library.path=".to_string() + libraries_path.to_str().unwrap())
        .arg("-Djna.tmpdir=".to_string() + libraries_path.to_str().unwrap())
        .arg("-Dio.netty.native.workdir=".to_string() + libraries_path.to_str().unwrap())
        .arg("-Dminecraft.launcher.brand=minecraft-launcher")
        .arg("-Dminecraft.launcher.version=1.20.4")
        .args(["-cp", &java_libraries])
        .args(["-Xmx2G", "-XX:+UnlockExperimentalVMOptions", "-XX:+UseG1GC", "-XX:G1NewSizePercent=20", "-XX:G1ReservePercent=20", "-XX:MaxGCPauseMillis=50", "-XX:G1HeapRegionSize=32M"])
        .arg("net.minecraft.client.main.Main")
        .args(["--username", &config.username])
        .args(["--version", "1.20.4"])
        .args(["--gameDir", parent_dir.to_str().unwrap()])
        .args(["--assetsDir", assets_path.to_str().unwrap()])
        .args(["--assetIndex", "12"])
        .args(["--accessToken"])
        .args(["--versionType", "release"])
        .spawn()
        .unwrap();
}

#[tokio::main]
async fn main() {
    let current_exe = env::current_exe().unwrap();

    let exe_name = current_exe
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    if exe_name == "blazinglyassmc.exe" {
        create_profile().await
    } else {
        launch_minecraft()
    }
}
