# Changelog

All notable changes to this project are documented in this file.

## [0.1.2] - 2026-07-14

### Other

- Cloudflare Images URL migration, workers-rs edge scaffold, dependency updates ([#23](https://github.com/officialunofficial/imgx/pull/23))(c043bb1)
- Merge branch 'main' of https://github.com/officialunofficial/imgx(a32190b)

### Refactoring

- *(router)* Drop cdn-cgi/ prefix from image request URLs(97dc636)


## [0.1.1] - 2026-07-10

### Bug Fixes

- *(vips)* Select AV1 compression explicitly for AVIF encoding ([#8](https://github.com/officialunofficial/imgx/pull/8))(bb21f83)
- Wire dead transform limits, bound origin fetch size, stop silent cache/transform failures ([#9](https://github.com/officialunofficial/imgx/pull/9))(c770fd7)

### CI/CD

- Security scanning, MSRV enforcement, edition 2024, Docker hardening, repo hygiene ([#11](https://github.com/officialunofficial/imgx/pull/11))(5c5965f)

### Features

- Replace custom-JSON /metrics with real Prometheus exposition format ([#12](https://github.com/officialunofficial/imgx/pull/12))(356e4c3)

### Miscellaneous

- Release v0.1.0 ([#7](https://github.com/officialunofficial/imgx/pull/7))(e2222b2)
- Make RUST_LOG actually work, add optional JSON log format ([#13](https://github.com/officialunofficial/imgx/pull/13))(be5b1d6)

### Testing

- Real HTTP-mocked status coverage, s3 client status mapping, fixture gaps ([#10](https://github.com/officialunofficial/imgx/pull/10))(3a36359)


## [0.1.0] - 2026-07-10

### Other

- Rewrite zimgx (Zig) as imgx (Rust) ([#6](https://github.com/officialunofficial/imgx/pull/6))(33ad233)

