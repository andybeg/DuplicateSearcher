fn main() {
    println!("cargo:rerun-if-changed=windows-manifest.xml");
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_manifest(include_str!("windows-manifest.xml"));
        res.compile().expect("не удалось встроить манифест Windows");
    }
}
