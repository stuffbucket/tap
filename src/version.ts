import packageJson from "../package.json" with { type: "json" }

/** Tap's version, from package.json; a release tag `vX.Y.Z` must match it. */
export const TAP_VERSION: string = packageJson.version
