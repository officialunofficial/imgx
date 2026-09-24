# Changelog

All notable changes to this project are documented in this file.


## [0.1.8] - 2026-09-24

### Bug Fixes

- *(vips)* Select AV1 compression explicitly for AVIF encoding ([#8](https://github.com/officialunofficial/imgx/pull/8))(bb21f83)
- *(transform)* Apply EXIF orientation to derived resize sides(ed78126)
- Satisfy stable clippy and libvips 8.15 in CI(60010f0)
- *(transform)* Fail format=thumbhash when the source decodes only in part(b2b7fbc)
- *(vips)* Keep source bytes owned and turn off the operation cache(e97a2c4)

### CI/CD

- Security scanning, MSRV enforcement, edition 2024, Docker hardening, repo hygiene ([#11](https://github.com/officialunofficial/imgx/pull/11))(5c5965f)

### Features

- *(transform)* Add thumbhash format, shrink-on-load resize, AVIF effort(ca6da0a)

### Miscellaneous

- Release v0.1.0 ([#7](https://github.com/officialunofficial/imgx/pull/7))(e2222b2)
- Release v0.1.1 ([#21](https://github.com/officialunofficial/imgx/pull/21))(28f29ec)
- Release v0.1.2(cc61197)
- Release v0.1.3(2c25d8d)
- Release v0.1.3(92af209)
- Release v0.1.4(0b200cd)
- Release v0.1.5(7f4cced)
- Release v0.1.6(4f3e8da)
- Release v0.1.7(b1fce42)

### Other

- Rewrite zimgx (Zig) as imgx (Rust) ([#6](https://github.com/officialunofficial/imgx/pull/6))(33ad233)
- Cloudflare Images URL migration, workers-rs edge scaffold, dependency updates ([#23](https://github.com/officialunofficial/imgx/pull/23))(c043bb1)
- Merge branch 'main' of https://github.com/officialunofficial/imgx(a32190b)

### Testing

- Real HTTP-mocked status coverage, s3 client status mapping, fixture gaps ([#10](https://github.com/officialunofficial/imgx/pull/10))(3a36359)
- *(vips)* Scan FFI-isolation guard test recursively ([#46](https://github.com/officialunofficial/imgx/pull/46))(16c061c)


## [0.1.7] - 2026-09-21

### Testing

- *(vips)* Scan FFI-isolation guard test recursively ([#46](https://github.com/officialunofficial/imgx/pull/46))(16c061c)


## [0.1.6] - 2026-09-20

### Bug Fixes

- *(vips)* Keep source bytes owned and turn off the operation cache(e97a2c4)


## [0.1.5] - 2026-09-19

### Bug Fixes

- *(transform)* Apply EXIF orientation to derived resize sides(ed78126)
- Satisfy stable clippy and libvips 8.15 in CI(60010f0)
- *(transform)* Fail format=thumbhash when the source decodes only in part(b2b7fbc)

### Features

- *(transform)* Add thumbhash format, shrink-on-load resize, AVIF effort(ca6da0a)


## [0.1.4] - 2026-07-14

### Miscellaneous

- Release v0.1.3(92af209)


## [0.1.3] - 2026-07-14

### Bug Fixes

- *(vips)* Select AV1 compression explicitly for AVIF encoding ([#8](https://github.com/officialunofficial/imgx/pull/8))(bb21f83)

### CI/CD

- Security scanning, MSRV enforcement, edition 2024, Docker hardening, repo hygiene ([#11](https://github.com/officialunofficial/imgx/pull/11))(5c5965f)

### Miscellaneous

- Release v0.1.0 ([#7](https://github.com/officialunofficial/imgx/pull/7))(e2222b2)
- Release v0.1.1 ([#21](https://github.com/officialunofficial/imgx/pull/21))(28f29ec)
- Release v0.1.2(cc61197)
- Release v0.1.3(2c25d8d)

### Other

- Rewrite zimgx (Zig) as imgx (Rust) ([#6](https://github.com/officialunofficial/imgx/pull/6))(33ad233)
- Cloudflare Images URL migration, workers-rs edge scaffold, dependency updates ([#23](https://github.com/officialunofficial/imgx/pull/23))(c043bb1)
- Merge branch 'main' of https://github.com/officialunofficial/imgx(a32190b)

### Testing

- Real HTTP-mocked status coverage, s3 client status mapping, fixture gaps ([#10](https://github.com/officialunofficial/imgx/pull/10))(3a36359)


## [0.1.2] - 2026-07-14

### Other

- Cloudflare Images URL migration, workers-rs edge scaffold, dependency updates ([#23](https://github.com/officialunofficial/imgx/pull/23))(c043bb1)
- Merge branch 'main' of https://github.com/officialunofficial/imgx(a32190b)


## [0.1.1] - 2026-07-10

### Bug Fixes

- *(vips)* Select AV1 compression explicitly for AVIF encoding ([#8](https://github.com/officialunofficial/imgx/pull/8))(bb21f83)

### CI/CD

- Security scanning, MSRV enforcement, edition 2024, Docker hardening, repo hygiene ([#11](https://github.com/officialunofficial/imgx/pull/11))(5c5965f)

### Miscellaneous

- Release v0.1.0 ([#7](https://github.com/officialunofficial/imgx/pull/7))(e2222b2)

### Testing

- Real HTTP-mocked status coverage, s3 client status mapping, fixture gaps ([#10](https://github.com/officialunofficial/imgx/pull/10))(3a36359)


## [0.1.0] - 2026-07-10

### Other

- Rewrite zimgx (Zig) as imgx (Rust) ([#6](https://github.com/officialunofficial/imgx/pull/6))(33ad233)

