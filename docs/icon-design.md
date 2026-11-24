# Tap icon design

## Current assets

The existing flattened assets remain unchanged:

- `assets/icon.svg` — editable flattened source.
- `assets/icon.png` — 1024×1024 fallback/marketing raster.
- `assets/Tap.icns` — current hand-built macOS bundle icon.

Reusable exact layers:

- `assets/icon-background.svg` — transparent 1024×1024 canvas containing the original rounded-square gradient and baked shadow only.
- `assets/icon-terminal-frame.svg` — transparent 1024×1024 canvas containing the terminal panel, controls, record indicator, prompt, and cursor/underscore.

Compositing the two layers reproduces the current flattened design. Both pass `xmllint --noout`.

## macOS 26 and 27

Apple does not define separate “style 26” and “style 27” app-icon asset formats. macOS 26 introduced the layered Liquid Glass icon language. macOS 27 refines the system material with sharper edge definition, revised shadows/translucency, vertical lighting, and additional specular/refraction controls. Existing Icon Composer documents receive the newer rendering automatically and can be previewed for back-deployment.

The production strategy should therefore be:

1. Keep one layered Icon Composer source.
2. Annotate Default, Dark, and Mono appearances.
3. Preview the same source with macOS 26 and current/macOS 27 rendering.
4. Tune material/refraction/specular controls by version in Icon Composer rather than maintaining divergent artwork.
5. Keep `Tap.icns` as the fallback for the current hand-built app bundle until packaging moves through Xcode/asset catalogs.

## Icon Composer treatment

The exact split assets above preserve the current icon, but the original background is already masked into a squircle and contains a baked drop shadow. Those choices are appropriate for the fallback `.icns`, not ideal as an Icon Composer background layer.

For an Icon Composer production source:

- Use a full-bleed 1024×1024 dark gradient background without an outer mask or baked shadow.
- Let the system apply the rounded-rectangle mask, material, highlights, and dynamic shadow.
- Import the terminal panel as a clear-edged foreground group.
- Keep the prompt chevron and baseline bold enough to survive 16px and 32px rendering.
- Use restrained or flat material for the prompt rather than baking glow or bevel effects.
- Test whether the traffic-light dots and record indicator remain legible at small sizes; simplify them if they merge.
- Avoid thin lines, detailed screenshots, static specular effects, and inter-layer shadows.
- Tune Default, Dark, and Mono explicitly; ensure the mono artwork spans a useful dark-to-light tonal range.
- Test Desktop, Dock, Spotlight, 16px, 32px, all six generated appearances, macOS 26 preview, and current/macOS 27 rendering.

## Build integration

`scripts/build-icon.ts` continues to rasterize `assets/icon.svg` into `assets/icon.png` and `assets/Tap.icns`. The current DMG uses `Tap.icns` via `CFBundleIconFile`.

A future Icon Composer `.icon` should be compiled through Xcode/asset-catalog tooling. Copying an `.icon` document next to `CFBundleIconFile` is not the documented deployment path.

## Apple references

- App icon HIG: https://developer.apple.com/design/human-interface-guidelines/app-icons/
- Icon Composer: https://developer.apple.com/icon-composer/
- WWDC25 — Say hello to the new look of app icons: https://developer.apple.com/videos/play/wwdc2025/220/
- WWDC25 — Create icons with Icon Composer: https://developer.apple.com/videos/play/wwdc2025/361/
- WWDC26 Platforms State of the Union: https://developer.apple.com/videos/play/wwdc2026/102/
- WWDC26 Icon Composer group lab: https://developer.apple.com/videos/play/wwdc2026/8012/
- Xcode Icon Composer guide: https://developer.apple.com/documentation/Xcode/creating-your-app-icon-using-icon-composer
- Apple design resources: https://developer.apple.com/design/resources/
- macOS 27 release notes: https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes
