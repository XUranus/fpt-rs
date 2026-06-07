/** @type {import('@docusaurus/plugin-content-docs').SidebarsConfig} */
const sidebars = {
  tutorialSidebar: [
    'intro',
    {
      type: 'category',
      label: 'Getting Started',
      items: ['guides/quick-start', 'guides/installation', 'guides/first-backup'],
    },
    {
      type: 'category',
      label: 'Architecture',
      items: [
        'architecture/overview',
        'architecture/module-structure',
        'architecture/data-flow',
        'architecture/transport-layer',
        'architecture/copy-layout',
      ],
    },
    {
      type: 'category',
      label: 'Core Concepts',
      items: [
        'concepts/scan-engine',
        'concepts/backup-pipeline',
        'concepts/restore-pipeline',
        'concepts/incremental-backup',
        'concepts/aggregation',
        'concepts/hardlinks',
        'concepts/control-files',
        'concepts/metadata-format',
      ],
    },
    {
      type: 'category',
      label: 'Transport Engines',
      items: [
        'transports/overview',
        'transports/local',
        'transports/nfs',
        'transports/smb',
        'transports/traits',
      ],
    },
    {
      type: 'category',
      label: 'Guides',
      items: [
        'guides/nfs-setup',
        'guides/smb-setup',
        'guides/performance-tuning',
        'guides/logging',
        'guides/failure-handling',
      ],
    },
    {
      type: 'category',
      label: 'CLI Reference',
      items: [
        'reference/fptcli',
        'reference/fptserver',
        'reference/fsscan',
        'reference/fsdiff',
        'reference/metainspect',
      ],
    },
  ],
};

module.exports = sidebars;
