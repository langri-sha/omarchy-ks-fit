import { Project } from '@langri-sha/projen-project'
import { JsonFile, TextFile } from 'projen'

const version = '0.1.0'
const pluginId = 'io.github.langri-sha.ks-fit'

const project = new Project({
  name: 'omarchy-ks-fit',
  package: {
    authorEmail: 'filip.dupanovic@gmail.com',
    authorName: 'Filip Dupanović',
    authorOrganization: false,
    authorUrl: 'https://langri-sha.com',
    bugsUrl: 'https://github.com/langri-sha/omarchy-ks-fit/issues',
    copyrightYear: '2026',
    description:
      'Omarchy plugin to control a KingSmith WalkingPad and chart your steps and calories',
    homepage: 'https://github.com/langri-sha/omarchy-ks-fit#readme',
    license: 'MIT',
    licensed: true,
    minNodeVersion: '24.16.0',
    repository: 'langri-sha/omarchy-ks-fit',
    type: 'module',

    autoDetectBin: false,
    entrypoint: '',
    devDeps: ['@langri-sha/lint-staged@0.9.11', '@langri-sha/prettier@0.4.11'],
  },
  cargo: {
    workspace: {
      package: {
        authors: ['Filip Dupanović <filip.dupanovic@gmail.com>'],
        license: 'MIT',
        publish: false,
        repository: 'https://github.com/langri-sha/omarchy-ks-fit',
        'rust-version': '1.89',
        version,
      },
      dependencies: {
        anyhow: '1.0.104',
        bluer: { version: '0.17.4', features: ['bluetoothd'] },
        'futures-util': '0.3.34',
        jiff: '0.2.37',
        log: '0.4.34',
        rusqlite: { version: '0.40.2', features: ['bundled'] },
        serde: { version: '1.0.229', features: ['derive'] },
        serde_json: '1.0.151',
        thiserror: '2.0.21',
        tokio: {
          version: '1.53.2',
          features: [
            'io-std',
            'io-util',
            'macros',
            'rt',
            'signal',
            'sync',
            'time',
          ],
        },
      },
      lints: {
        rust: {
          unsafe_code: 'forbid',
        },
      },
    },
    profile: {
      release: {
        'codegen-units': 1,
        lto: true,
        strip: true,
      },
    },
    toolchain: {
      components: ['clippy', 'rustfmt'],
    },
  },
  codeowners: {
    '*': '@langri-sha',
  },
  editorConfig: {
    // Listed first so the preset's defaults land here rather than after the
    // Rust section, where they would override its indent_size.
    '*': {},
    '*.rs': {
      indent_size: 4,
    },
  },
  husky: {
    'pre-commit': 'lint-staged',
  },
  lintStaged: {
    config: {
      '*.rs': 'rustfmt',
    },
  },
  lintSynthesized: {},
  pnpmWorkspace: {
    minimumReleaseAgeExclude: ['@langri-sha/*'],
  },
  prettier: {},
  readme: {
    filename: 'readme.md',
  },
  renovate: {
    packageRules: [
      {
        description: 'Update our own packages together',
        groupName: 'langri-sha projen toolchain',
        groupSlug: 'langri-sha-projen',
        matchPackageNames: ['@langri-sha/**'],
      },
      {
        description: 'Install our own packages without waiting them out',
        matchPackageNames: ['@langri-sha/**'],
        minimumReleaseAge: null,
      },
      {
        description:
          'Install our own GitHub Actions and Terraform modules without waiting them out',
        matchPackageNames: ['langri-sha/**'],
        minimumReleaseAge: null,
      },
    ],
  },
  worktrunk: {
    config: {
      'pre-start': [
        {
          sync: '{% if base == default_branch %}git fetch {{ remote }} {{ default_branch }} && git merge --ff-only --quiet {{ remote }}/{{ default_branch }}{% endif %}',
        },
        {
          // `pnpm i`: the plugin marketplace scans .toml files and holds a
          // listing for review when it finds pnpm's install command spelled
          // out.
          install: 'pnpm i --frozen-lockfile',
        },
      ],
    },
  },
})

project.addSubproject({
  name: 'ks-fit',
  outdir: 'daemon',
  cargo: {
    package: {
      description:
        'Drives a KingSmith WalkingPad over Bluetooth LE and records the walks',
    },
    dependencies: {
      anyhow: { workspace: true },
      bluer: { workspace: true },
      'futures-util': { workspace: true },
      jiff: { workspace: true },
      log: { workspace: true },
      rusqlite: { workspace: true },
      serde: { workspace: true },
      serde_json: { workspace: true },
      thiserror: { workspace: true },
      tokio: { workspace: true },
    },
    'dev-dependencies': {
      tokio: { workspace: true, features: ['test-util'] },
    },
    lints: {
      workspace: true,
    },
  },
})

new JsonFile(project, 'manifest.json', {
  obj: {
    schemaVersion: 1,
    id: pluginId,
    name: 'KS Fit',
    version,
    author: 'Filip Dupanović',
    license: 'MIT',
    description:
      'Control a KingSmith WalkingPad from the bar and chart your steps and calories.',
    kinds: ['service', 'bar-widget'],
    keepLoaded: true,
    entryPoints: {
      service: 'shell/Service.qml',
      barWidget: 'shell/BarWidget.qml',
    },
    barWidget: {
      displayName: 'KS Fit',
      description:
        'WalkingPad control, live workout and step and calorie history',
      category: 'Hardware',
      allowMultiple: false,
      defaultSection: 'center',
      defaults: {
        weightKg: 70,
        demo: false,
      },
      schema: [
        {
          key: 'weightKg',
          type: 'integer',
          label: 'Body weight (kg)',
          min: 20,
          max: 300,
          step: 1,
          defaultValue: 70,
        },
        {
          key: 'demo',
          type: 'boolean',
          label: 'Demo mode (simulated pad)',
          defaultValue: false,
        },
      ],
    },
  },
})

// The service compares this with the manifest the shell hands it: after
// `omarchy plugin update`, a kept service still runs the old QML.
const versionFile = new TextFile(project, 'shell/Version.js')
versionFile.addLine('.pragma library')
versionFile.addLine(`// ${versionFile.marker}`)
versionFile.addLine('')
versionFile.addLine(`var version = "${version}"`)
versionFile.addLine('')

// Prettier can't parse the `.pragma library` line QML needs.
project.prettier?.ignore.addPatterns('/shell/Model.js', '/shell/Version.js')

project.package?.addField('private', true)
project.package?.addField('packageManager', 'pnpm@12.8.2')
project.package?.addEngine('pnpm', '>= 11.0.0')

project.package?.setScript('format', 'prettier --write .')
project.package?.setScript('format:check', 'prettier --check .')

project.testTask.exec('cargo test --locked')
project.testTask.exec("node --test 'tests/**/*.test.js'")
project.package?.setScript('test', 'pnpm exec projen test')

project.addTask('qmllint', {
  description: 'Lint the plugin QML against the installed Omarchy shell',
  exec: 'scripts/qmllint.sh',
})

project.addTask('plugin:render', {
  description:
    'Render the plugin offscreen against a mock helper (KS_FIT_RENDER_HELPER picks another)',
  exec: 'tests/render/render.sh',
})

project.addTask('plugin:validate', {
  description:
    'Validate the plugin as `omarchy plugin add` would clone it, without node_modules',
  exec: 'scripts/validate-plugin.sh',
})

project.annotateGenerated('/Cargo.lock')

project.gitattributes.addAttributes(
  'readme',
  'text=auto',
  'linguist-language=Markdown',
)

project.synth()
