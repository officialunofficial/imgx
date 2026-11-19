# Image engine golden fixtures

- `blue-marble.jpg` is NASA's Blue Marble western hemisphere composite (2002),
  public domain as a work of the United States government. The source file
  and attribution are recorded on [Wikimedia Commons](https://commons.wikimedia.org/wiki/File:Earth_Western_Hemisphere.jpg)
  and [NASA Visible Earth](https://visibleearth.nasa.gov/images/57723/the-blue-marble-land-surface-ocean-color-sea-ice-and-clouds).
- The other inputs are generated colour ramps and geometric shapes, dedicated
  to the public domain under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/).
  `portrait-icc.jpg` stores odd landscape dimensions, orientation 6 and a
  generated sRGB profile. `cmyk.jpg` stores four colour bands without alpha.
  `transparent.png` and `small.png` contain an alpha ramp.
  `animated.gif` has two visually different frames.
  `corrupt.png` is the first half of `transparent.png`, so it retains the
  header and fails during strict pixel decode.

The HTTP golden compares the native ladder from the original pixels with the
legacy renderer reading the encoded baseline. The six measured minimum RGB SSIM
scores on the pinned image were 0.96731, 0.97647, 0.96906, 0.92196, 1.00000 and
0.97550 for Blue Marble, portrait, CMYK, transparent, small and animation,
respectively. The maximum mean alpha error was 0.175 on the 0–255 scale.
The threshold for alpha is 0.5. The test also compares the HTTP bytes to
a standalone render with the same encoded input and reduced decode policy.

Set `IMGX_PINNED_GOLDENS=1` in the pinned CI image to also assert SHA-256
goldens for the Q95 photo master and the lossless transparent master.
