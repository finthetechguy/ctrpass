use std::path::Path;

use clap::Subcommand;
use ctrpass::identity::Identity;
use ctrpass::plugin;
use ctrpass::title::Context;
use serde_json::json;

use crate::title::about;

#[derive(Subcommand)]
pub enum Action {
    /// List the loaded plugins, and those that failed to load, as JSON.
    List,
    /// Load a plugin file and make, read and rewrite a body with it, without installing it.
    Check { file: std::path::PathBuf },
}

pub fn list() {
    let r = plugin::registry();
    let out = json!({
        "folder": plugin::default_dir(),
        "plugins": r.all().iter().map(about).collect::<Vec<_>>(),
        "errors": r.errors().iter().map(|e| json!({
            "file": e.file.display().to_string(),
            "error": e.error,
        })).collect::<Vec<_>>(),
        "warnings": r.warnings(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

pub fn check(file: &Path) -> Result<(), String> {
    let t = plugin::load_file(file).map_err(|e| format!("{}: {e}", file.display()))?;
    // Nothing is signed here, so a made-up identity and Mii key are enough.
    let id = Identity::from_seed(&[0x5a; 32]);
    let key = [0x31; 16];
    let ctx = Context {
        record_key: Some(&key),
        identity: Some(&id),
    };
    let step = |what: &str, e: ctrpass::Error| format!("{}: {what}: {e}", t.name());
    let body = t.create(&ctx).map_err(|e| step("create", e))?;
    let values = t.read(&body, &ctx).map_err(|e| step("read", e))?;
    let again = t
        .write(&body, &values, &ctx)
        .map_err(|e| step("write", e))?;
    let problems = t.problems(&body);
    let mut out = about(&t);
    out["fields"] = t.fields().len().into();
    out["body_size"] = body.len().into();
    out["round_trip"] = (again == body).into();
    out["problems"] = json!(problems);
    out["values"] = values.into();
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    if again != body {
        return Err("writing the values read back changed the body".into());
    }
    if !problems.is_empty() {
        return Err("a new body has problems".into());
    }
    Ok(())
}
