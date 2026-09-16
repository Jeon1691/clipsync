pub fn emit(json: bool, value: serde_json::Value) {
    if json {
        println!("{}", value);
    }
}

pub fn emit_err(json: bool, message: &str) {
    if json {
        println!(
            "{}",
            serde_json::json!({"ok": false, "error": {"message": message}})
        );
    } else {
        eprintln!("{message}");
    }
}
