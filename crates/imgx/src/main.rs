#![forbid(unsafe_code)]

fn main() {
    let cfg = imgx::config::Config::load_from_env().expect("invalid configuration");
    cfg.validate().expect("invalid configuration");

    imgx_vips::init().expect("failed to initialize libvips");
    println!("imgx scaffold — config loaded and validated, vips linked");
    imgx_vips::shutdown();
}
