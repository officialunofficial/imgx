fn main() {
    pkg_config::Config::new()
        .atleast_version("8.14")
        .probe("vips")
        .expect("libvips >= 8.14 not found via pkg-config; install libvips-dev / vips-dev");
}
