import { Project } from '@langri-sha/projen-project'

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

    entrypoint: '',
    devDeps: ['@langri-sha/lint-staged@0.9.11', '@langri-sha/prettier@0.4.11'],
  },
  codeowners: {
    '*': '@langri-sha',
  },
  editorConfig: {},
  husky: {
    'pre-commit': 'lint-staged',
  },
  lintStaged: {},
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
          install: 'pnpm install --frozen-lockfile',
        },
      ],
    },
  },
})

project.package?.addField('private', true)
project.package?.addField('packageManager', 'pnpm@12.8.2')
project.package?.addEngine('pnpm', '>= 11.0.0')

project.package?.setScript('format', 'prettier --write .')
project.package?.setScript('format:check', 'prettier --check .')

project.gitattributes.addAttributes(
  'readme',
  'text=auto',
  'linguist-language=Markdown',
)

project.synth()
