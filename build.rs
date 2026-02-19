fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS")
        .map(|s| s == "windows")
        .unwrap_or(false)
    {
        thunk::thunk();

        let mut res = winres::WindowsResource::new();

        let name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
        let desc = std::env::var("CARGO_PKG_DESCRIPTION").unwrap_or_default();
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
        let authors = std::env::var("CARGO_PKG_AUTHORS").unwrap_or_default();
        let license = std::env::var("CARGO_PKG_LICENSE").unwrap_or_default();

        res.set("FileVersion", &version);
        res.set("ProductName", &name);
        res.set("ProductVersion", &version);

        if !desc.is_empty() {
            res.set("FileDescription", &desc);
        }
        if !authors.is_empty() {
            res.set("CompanyName", &authors);
        }
        if !license.is_empty() {
            res.set("LegalCopyright", &license);
        }

        if let Err(e) = res.compile() {
            eprintln!("[build.rs] failed to compile Windows resources: {}", e);
        }
    }
}
