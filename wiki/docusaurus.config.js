// @ts-check
const {themes} = require('prism-react-renderer');
const lightCodeTheme = themes.github;
const darkCodeTheme = themes.dracula;

/** @type {import('@docusaurus/types').Config} */
const config = {
  title: 'Fpt Backup Engine',
  tagline: 'High-performance, cross-platform file backup and recovery',
  favicon: 'img/favicon.ico',
  url: 'https://xuranus.github.io',
  baseUrl: '/fpt-rs/',
  organizationName: 'XUranus',
  projectName: 'fpt-rs',
  onBrokenLinks: 'warn',
  onBrokenMarkdownLinks: 'warn',
  i18n: { defaultLocale: 'en', locales: ['en', 'zh-CN'] },
  markdown: { mermaid: true },
  themes: ['@docusaurus/theme-mermaid'],
  presets: [
    ['classic', /** @type {import('@docusaurus/preset-classic').Options} */ ({
      docs: {
        sidebarPath: require.resolve('./sidebars.js'),
        editUrl: 'https://github.com/XUranus/fpt-rs/tree/master/wiki/',
      },
      theme: { customCss: require.resolve('./src/css/custom.css') },
    })],
  ],
  themeConfig: /** @type {import('@docusaurus/preset-classic').ThemeConfig} */ ({
    navbar: {
      title: 'Fpt',
      items: [
        { type: 'doc', docId: 'intro', position: 'left', label: 'Docs' },
        { to: '/docs/architecture/overview', label: 'Architecture', position: 'left' },
        { to: '/docs/guides/quick-start', label: 'Guides', position: 'left' },
        { href: 'https://github.com/XUranus/fpt-rs', label: 'GitHub', position: 'right' },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        { title: 'Docs', items: [
          { label: 'Getting Started', to: '/docs/guides/quick-start' },
          { label: 'Architecture', to: '/docs/architecture/overview' },
          { label: 'CLI Reference', to: '/docs/reference/fptcli' },
        ]},
        { title: 'Community', items: [
          { label: 'GitHub', href: 'https://github.com/XUranus/fpt-rs' },
          { label: 'Issues', href: 'https://github.com/XUranus/fpt-rs/issues' },
        ]},
      ],
      copyright: `Copyright © ${new Date().getFullYear()} XUranus. Built with Docusaurus.`,
    },
    prism: {
      theme: lightCodeTheme,
      darkTheme: darkCodeTheme,
      additionalLanguages: ['rust', 'bash', 'toml'],
    },
  }),
};

module.exports = config;
