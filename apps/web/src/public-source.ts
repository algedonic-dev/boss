// A browser reads no host env file. The owning mirror pin holds this
// single browser definition equal to infra/estate/estate.toml; consumers
// derive their links here rather than copying the repository address.
export const PUBLIC_MIRROR_URL = 'https://github.com/algedonic-dev/boss';
export const STEP_PLUGIN_README_URL = `${PUBLIC_MIRROR_URL}/blob/main/infra/step-plugins/README.md`;
