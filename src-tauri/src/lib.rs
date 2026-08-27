use cc_switch_core::AppType;

fn lite_apps() -> [AppType; 2] {
    [AppType::Claude, AppType::Codex]
}

#[tauri::command]
fn supported_apps() -> Vec<String> {
    lite_apps()
        .into_iter()
        .map(|app| app.as_str().to_owned())
        .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![supported_apps])
        .run(tauri::generate_context!())
        .expect("failed to run CC Switch Lite");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lite_boundary_contains_only_claude_and_codex() {
        assert_eq!(supported_apps(), ["claude", "codex"]);
    }
}
