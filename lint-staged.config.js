import defaults from '@langri-sha/lint-staged'

/** @type {import('lint-staged').Configuration} */
const config = {
  ...defaults,
  '*.rs': 'rustfmt',
}

export default config
