fn main() {
    println!("cargo:rerun-if-changed=icon.png");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let source = image::open("icon.png").expect("Cannot load application icon");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Missing OUT_DIR"));
    let icon = output.join("app.ico");
    let frames: Vec<_> = [16, 24, 32, 48, 64, 128, 256]
        .into_iter()
        .map(|size| {
            let pixels = source.resize_exact(size, size, image::imageops::FilterType::Lanczos3).to_rgba8();
            image::codecs::ico::IcoFrame::as_png(&pixels, size, size, image::ExtendedColorType::Rgba8)
                .expect("Cannot encode icon frame")
        })
        .collect();
    image::codecs::ico::IcoEncoder::new(std::fs::File::create(&icon).expect("Cannot create ICO"))
        .encode_images(&frames).expect("Cannot encode ICO");
    winresource::WindowsResource::new()
        .set_icon(icon.to_str().expect("Invalid icon path"))
        .set("ProductName", "APFS Explorer")
        .set("FileDescription", "APFS Explorer for Windows")
        .compile().expect("Cannot compile Windows icon resource");
}