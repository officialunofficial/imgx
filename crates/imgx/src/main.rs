fn main() {
    imgx_vips::init().expect("failed to initialize libvips");
    println!("imgx scaffold — workspace compiles, vips linked");
    imgx_vips::shutdown();
}
