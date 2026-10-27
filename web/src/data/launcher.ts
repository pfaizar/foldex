// What the page's launcher can find. The names are the demo folder used for
// the recordings; the groups, actions and order follow the real launcher.

export type Kind = 'folder' | 'file' | 'app' | 'window' | 'action';
export type Group =
  'Favorites' | 'Recents' | 'Apps' | 'Files' | 'Windows' | 'Actions';

export interface Entry {
  title: string;
  detail: string;
  kind: Kind;
  group: Group;
  /** File type badge, or the app tile's glyph. */
  mark?: string;
  /** The app tile's two colours. */
  tint?: [string, string];
  /** For windows: which context on the desk it brings forward. */
  context?: number;
}

const folder = (title: string, detail: string, group: Group): Entry => ({
  title,
  detail,
  kind: 'folder',
  group,
});
const file = (
  title: string,
  detail: string,
  group: Group = 'Files',
): Entry => ({
  title,
  detail,
  kind: 'file',
  group,
  mark: title.split('.').pop()!.toUpperCase(),
});
const app = (title: string, mark: string, tint: [string, string]): Entry => ({
  title,
  detail: 'Installed app',
  kind: 'app',
  group: 'Apps',
  mark,
  tint,
});

export const favorites: Entry[] = [
  folder('Studio', 'FoldexDemo', 'Favorites'),
  folder('Design', 'FoldexDemo / Studio', 'Favorites'),
  folder('Development', 'FoldexDemo / Studio', 'Favorites'),
  folder('References', 'FoldexDemo / Studio', 'Favorites'),
];

export const recents: Entry[] = [
  file('launch-plan.md', 'Studio', 'Recents'),
  file('budget-2026.xlsx', 'Studio', 'Recents'),
  file('team-photo.jpg', 'Studio / Brand', 'Recents'),
];

export const entries: Entry[] = [
  ...favorites,
  ...recents,
  app('Windows Terminal', '>_', ['#22272b', '#4a5258']),
  app('Notepad', 'N', ['#46525e', '#8193a3']),
  app('Paint', 'P', ['#7a5242', '#c99a78']),
  app('Photos', 'Ph', ['#2f3d39', '#6f8f84']),
  app('Snipping Tool', 'S', ['#3b444d', '#7b8a96']),
  app('Task Manager', 'T', ['#28211e', '#6d5c52']),
  app('Settings', '⚙', ['#4a4f55', '#9aa1a6']),
  folder('Downloads', 'This PC', 'Files'),
  folder('Documents', 'This PC', 'Files'),
  folder('Brand', 'FoldexDemo / Studio', 'Files'),
  folder('Releases', 'FoldexDemo / Studio', 'Files'),
  folder('Meeting notes', 'FoldexDemo / Studio', 'Files'),
  file('roadmap.xlsx', 'Studio'),
  file('quarterly-review.pptx', 'Studio'),
  file('notes.txt', 'Studio'),
  file('README.md', 'Studio / Development'),
  file('invoice-0412.pdf', 'Documents'),
  file('specs.pdf', 'Studio / References'),
  {
    title: 'main.rs — Code',
    detail: 'Open window',
    kind: 'window',
    group: 'Windows',
    context: 0,
  },
  {
    title: 'Q3 review — Slides',
    detail: 'Open window',
    kind: 'window',
    group: 'Windows',
    context: 1,
  },
  {
    title: 'budget-2026.xlsx — Sheets',
    detail: 'Open window',
    kind: 'window',
    group: 'Windows',
    context: 2,
  },
  {
    title: 'Weekly sync — Call',
    detail: 'Open window',
    kind: 'window',
    group: 'Windows',
    context: 3,
  },
  {
    title: 'Open Foldex',
    detail: 'Show your explorer workspaces',
    kind: 'action',
    group: 'Actions',
  },
  {
    title: 'Search files',
    detail: 'Find files in your user folders',
    kind: 'action',
    group: 'Actions',
  },
  {
    title: 'Clipboard history',
    detail: 'Find and copy recent text',
    kind: 'action',
    group: 'Actions',
  },
  {
    title: 'Switch window',
    detail: 'Focus an open window',
    kind: 'action',
    group: 'Actions',
  },
  {
    title: 'Launcher settings',
    detail: 'Shortcut, tray mode, and search folders',
    kind: 'action',
    group: 'Actions',
  },
];

/** Suggestions under the desk, each typed into the launcher when clicked. */
export const tries = [
  '1920*1080/1024',
  '12 km to mi',
  'sqrt(81)',
  'budget',
  'term',
  'call',
];
