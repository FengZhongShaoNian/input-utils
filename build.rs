use std::path::PathBuf;

fn main() {
    let project_root = std::env::current_dir().unwrap().to_str().unwrap().to_string();
    let config_file_path = PathBuf::from(&project_root).join("src").join("config.toml");
    let output = PathBuf::from(&project_root).join("target").join("debug").join("config.toml");
    std::fs::copy(config_file_path, output).unwrap();
}
