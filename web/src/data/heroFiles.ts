// The files that drift behind the hero. Every one is different: folders in
// several colours, some with an emblem; documents drawn by type; pictures and
// clips with their own little scenes. The hero deals them out without
// repeats, so a file appears once per strip.

// Stroke icons for folder emblems and document glyphs, on a 24 unit grid.
const icons = {
  image: 'M4 4h16v16H4V4Zm0 11 5-5 5 5 3-3 3 3M15 8h.01',
  code: 'm8 7-5 5 5 5m8-10 5 5-5 5',
  download: 'M12 3v12m-5-5 5 5 5-5M5 16v4h14v-4',
  music:
    'M9 18V5l12-2v13M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0Zm12-2a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z',
  star: 'm12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2-5.6-3-5.6 3 1.1-6.2L3 9.6l6.2-.9L12 3Z',
  heart: 'M12 20s-7-4.4-7-10a4 4 0 0 1 7-2.6A4 4 0 0 1 19 10c0 5.6-7 10-7 10Z',
  clock: 'M12 8v5l3 2m6-3a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z',
  zap: 'M13 3 5 13h6l-1 8 8-10h-6l1-8Z',
  package: 'm21 8-9-5-9 5v8l9 5 9-5V8Zm-18 0 9 5 9-5m-9 5v8',
  palette:
    'M12 3a9 9 0 0 0 0 18c1.1 0 1.5-.8 1.5-1.5 0-.9-.6-1.3-.6-2.2 0-1 .8-1.8 1.8-1.8H17a4 4 0 0 0 4-4c0-4.7-4-8.5-9-8.5ZM7.5 11h.01M10 7.5h.01M14.5 7.5h.01',
  book: 'M4 4h6a2 2 0 0 1 2 2v14a2 2 0 0 0-2-2H4V4Zm16 0h-6a2 2 0 0 0-2 2v14a2 2 0 0 1 2-2h6V4Z',
  home: 'm3 10 9-7 9 7m-16-1v12h14V9m-10 12v-8h6v8',
  lock: 'M6 11h12v10H6V11Zm2 0V8a4 4 0 1 1 8 0v3',
  plane: 'M2 13l20-8-6 16-4-7-10-1Zm10 1 4-4',
  camera: 'M4 8h3l2-3h6l2 3h3v11H4V8Zm8 9a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z',
  terminal: 'M4 5h16v14H4V5Zm3 4 3 3-3 3m5 0h5',
} as const;
export type IconName = keyof typeof icons;

/** A CSS mask for an icon, drawn in black. */
export const iconMask = (name: IconName) =>
  `url("data:image/svg+xml,${encodeURIComponent(
    `<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' stroke-width='2.2' stroke-linecap='round' stroke-linejoin='round'><path d='${icons[name]}'/></svg>`,
  )}")`;

// Back and front of each folder colour.
const folderColours = {
  yellow: ['#d9a63c', '#f5c859'],
  blue: ['#3f7fc4', '#6ea9ea'],
  green: ['#379668', '#62c48f'],
  pink: ['#cc5287', '#f08bb4'],
  purple: ['#7458c6', '#a58cf0'],
  graphite: ['#656c74', '#9aa1a8'],
  orange: ['#d4702a', '#f5a15a'],
  teal: ['#1d8aa1', '#4cc0d6'],
} as const;
type FolderColour = keyof typeof folderColours;

export const folderArt = (colour: FolderColour) => {
  const [back, front] = folderColours[colour];
  return `url("data:image/svg+xml,${encodeURIComponent(
    `<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 52'><path d='M2 6a4 4 0 0 1 4-4h17l6 6h29a4 4 0 0 1 4 4v6H2z' fill='${back}'/><path d='M2 15h60v31a4 4 0 0 1-4 4H6a4 4 0 0 1-4-4z' fill='${front}'/><path d='M2 15h60v1.6H2z' fill='white' fill-opacity='.35'/></svg>`,
  )}")`;
};
export const folderInk = (colour: FolderColour) => folderColours[colour][0];

/** How a document's page is drawn. */
export type Glyph =
  | 'lines'
  | 'heading'
  | 'grid'
  | 'code'
  | 'slide'
  | 'zip'
  | 'play'
  | 'wave'
  | 'font'
  | 'date'
  | 'mixed'
  | 'shapes'
  | 'braces';

// Little scenes for pictures, drawn with gradients.
export const scenes = [
  // Sunset over the sea.
  'radial-gradient(circle at 50% 50%, #ffe3a3 0 15%, transparent 16%), linear-gradient(#f7b267 0%, #f4845f 50%, #7c4d8a 50%, #3d2f5b 100%)',
  // Mountains.
  'linear-gradient(135deg, transparent 50%, #3e5566 50%) 0 100% / 62% 72% no-repeat, linear-gradient(225deg, transparent 50%, #5d7689 50%) 100% 100% / 70% 86% no-repeat, linear-gradient(#a9d3f0, #e6f2f8)',
  // A bright day at the beach.
  'radial-gradient(circle at 76% 26%, #fff6d8 0 10%, transparent 11%), linear-gradient(#8fd3f4 0 56%, #2b8bc6 56% 70%, #f1d7a8 70%)',
  // Forest hills.
  'radial-gradient(60% 70% at 18% 108%, #2f6b4a 0 97%, transparent 100%), radial-gradient(70% 80% at 78% 112%, #3f8f5f 0 97%, transparent 100%), linear-gradient(#d6efdc, #a8d6b8)',
  // Night sky.
  'radial-gradient(circle at 72% 30%, #fdf6d8 0 11%, transparent 12%), radial-gradient(circle at 20% 30%, #fff 0 2%, transparent 3%), radial-gradient(circle at 42% 16%, #fff 0 1.6%, transparent 2.4%), radial-gradient(circle at 56% 52%, #fff 0 1.4%, transparent 2.2%), radial-gradient(circle at 12% 70%, #fff 0 1.4%, transparent 2.2%), linear-gradient(#14213d, #2c3e70)',
  // Portrait.
  'radial-gradient(circle at 50% 40%, #e8b48f 0 17%, transparent 18%), radial-gradient(36% 32% at 50% 102%, #5468a8 0 97%, transparent 100%), linear-gradient(#f6dfcd, #e9c3aa)',
  // Bright shapes.
  'radial-gradient(circle at 26% 32%, #ff7eb6 0 22%, transparent 23%), radial-gradient(circle at 70% 66%, #7ec8ff 0 26%, transparent 27%), radial-gradient(circle at 62% 20%, #ffd166 0 12%, transparent 13%), #f6efe6',
  // Dunes.
  'radial-gradient(90% 46% at 28% 104%, #d49456 0 97%, transparent 100%), radial-gradient(80% 54% at 88% 108%, #e8b878 0 97%, transparent 100%), linear-gradient(#ffcf96, #ffecd1)',
  // A city at dusk.
  'linear-gradient(#1d2b44, #1d2b44) 8% 100% / 18% 58% no-repeat, linear-gradient(#26375a, #26375a) 32% 100% / 16% 80% no-repeat, linear-gradient(#1a2740, #1a2740) 58% 100% / 22% 48% no-repeat, linear-gradient(#2c3f66, #2c3f66) 88% 100% / 16% 70% no-repeat, radial-gradient(circle at 70% 30%, #ffd2a1 0 8%, transparent 9%), linear-gradient(#ff9a76, #6b5b95)',
  // Pastel swirl.
  'conic-gradient(from 200deg at 60% 40%, #a1c4fd, #c2e9fb, #fbc2eb, #a6c1ee, #a1c4fd)',
  // Aurora.
  'radial-gradient(70% 30% at 40% 42%, #3ef2b0aa, transparent), radial-gradient(60% 26% at 66% 30%, #8a5cf6aa, transparent), linear-gradient(#071a2c, #0d2b45)',
  // A leaf, close up.
  'radial-gradient(28% 62% at 50% 52%, #5fae52 0 96%, transparent 100%), linear-gradient(90deg, transparent 49%, #3d7a35 49% 51%, transparent 51%) 0 0 / 100% 88% no-repeat, linear-gradient(90deg, #eef7e6, #d7ecc9)',
  // Snowy field.
  'radial-gradient(90% 40% at 50% 104%, #fff 0 97%, transparent 100%), linear-gradient(#3c6e5a, #3c6e5a) 30% 70% / 6% 24% no-repeat, linear-gradient(#bcd4e6, #e8f0f6)',
  // Tiles.
  'conic-gradient(#f4a261 25%, #2a9d8f 0 50%, #e9c46a 0 75%, #264653 0) 0 0 / 50% 50%',
  // Stripes.
  'linear-gradient(120deg, #ff6b6b 0 20%, #ffd93d 20% 40%, #6bcb77 40% 60%, #4d96ff 60% 80%, #9b5de5 80%)',
  // Coral sky.
  'radial-gradient(circle at 30% 70%, #fff3 0 18%, transparent 19%), linear-gradient(160deg, #ff9a8b, #ff6a88 55%, #ff99ac)',
];

export type HeroFile =
  | { kind: 'folder'; name: string; colour: FolderColour; emblem?: IconName }
  | { kind: 'doc'; name: string; ext: string; colour: string; glyph: Glyph }
  | { kind: 'app'; name: string; colour: string; icon: IconName }
  | {
      kind: 'image';
      name: string;
      scene: number;
      shape?: 'tall' | 'square';
      video?: boolean;
    };

export const heroFiles: HeroFile[] = [
  { kind: 'folder', name: 'Design', colour: 'yellow', emblem: 'palette' },
  { kind: 'folder', name: 'Development', colour: 'blue', emblem: 'code' },
  { kind: 'folder', name: 'Photos', colour: 'yellow', emblem: 'camera' },
  { kind: 'folder', name: 'Music', colour: 'pink', emblem: 'music' },
  { kind: 'folder', name: 'Downloads', colour: 'teal', emblem: 'download' },
  { kind: 'folder', name: 'Favourites', colour: 'orange', emblem: 'star' },
  { kind: 'folder', name: 'Travel', colour: 'green', emblem: 'plane' },
  { kind: 'folder', name: 'Private', colour: 'graphite', emblem: 'lock' },
  { kind: 'folder', name: 'Reading list', colour: 'purple', emblem: 'book' },
  { kind: 'folder', name: 'Home', colour: 'yellow', emblem: 'home' },
  { kind: 'folder', name: 'Brand', colour: 'yellow' },
  { kind: 'folder', name: 'Releases', colour: 'yellow' },
  { kind: 'folder', name: 'Invoices', colour: 'yellow' },
  { kind: 'folder', name: 'Archive', colour: 'graphite' },
  { kind: 'folder', name: 'Meeting notes', colour: 'blue' },
  { kind: 'folder', name: 'Wedding', colour: 'pink', emblem: 'heart' },
  { kind: 'folder', name: 'Scripts', colour: 'purple', emblem: 'terminal' },
  { kind: 'folder', name: 'Screenshots', colour: 'teal', emblem: 'image' },
  { kind: 'folder', name: 'Exports', colour: 'orange' },
  { kind: 'folder', name: 'Recipes', colour: 'green' },
  { kind: 'folder', name: 'Old projects', colour: 'graphite', emblem: 'clock' },
  { kind: 'folder', name: 'Packages', colour: 'yellow', emblem: 'package' },
  { kind: 'folder', name: 'Ideas', colour: 'orange', emblem: 'zap' },
  { kind: 'folder', name: 'References', colour: 'yellow' },

  {
    kind: 'doc',
    name: 'README.md',
    ext: 'MD',
    colour: '#3b82c4',
    glyph: 'heading',
  },
  {
    kind: 'doc',
    name: 'launch-plan.md',
    ext: 'MD',
    colour: '#3b82c4',
    glyph: 'lines',
  },
  {
    kind: 'doc',
    name: 'Cargo.toml',
    ext: 'TOML',
    colour: '#b5652b',
    glyph: 'code',
  },
  {
    kind: 'doc',
    name: 'theme.toml',
    ext: 'TOML',
    colour: '#9c4f8a',
    glyph: 'code',
  },
  {
    kind: 'doc',
    name: 'budget-2026.xlsx',
    ext: 'XLSX',
    colour: '#1f8f55',
    glyph: 'grid',
  },
  {
    kind: 'doc',
    name: 'roadmap.xlsx',
    ext: 'XLSX',
    colour: '#1f8f55',
    glyph: 'grid',
  },
  {
    kind: 'doc',
    name: 'contacts.csv',
    ext: 'CSV',
    colour: '#3c9a8c',
    glyph: 'grid',
  },
  {
    kind: 'doc',
    name: 'quarterly-review.pptx',
    ext: 'PPTX',
    colour: '#d0582c',
    glyph: 'slide',
  },
  {
    kind: 'doc',
    name: 'pitch.key',
    ext: 'KEY',
    colour: '#2f7ad8',
    glyph: 'slide',
  },
  {
    kind: 'doc',
    name: 'specs.pdf',
    ext: 'PDF',
    colour: '#d64541',
    glyph: 'mixed',
  },
  {
    kind: 'doc',
    name: 'invoice-0412.pdf',
    ext: 'PDF',
    colour: '#d64541',
    glyph: 'lines',
  },
  {
    kind: 'doc',
    name: 'cover-letter.docx',
    ext: 'DOCX',
    colour: '#2b5fb8',
    glyph: 'mixed',
  },
  {
    kind: 'doc',
    name: 'notes.txt',
    ext: 'TXT',
    colour: '#6b7176',
    glyph: 'lines',
  },
  { kind: 'doc', name: 'main.rs', ext: 'RS', colour: '#c4572b', glyph: 'code' },
  {
    kind: 'doc',
    name: 'app.tsx',
    ext: 'TSX',
    colour: '#2f74c0',
    glyph: 'code',
  },
  {
    kind: 'doc',
    name: 'build.py',
    ext: 'PY',
    colour: '#3a6fa8',
    glyph: 'code',
  },
  {
    kind: 'doc',
    name: 'styles.css',
    ext: 'CSS',
    colour: '#7b4fd0',
    glyph: 'code',
  },
  {
    kind: 'doc',
    name: 'settings.json',
    ext: 'JSON',
    colour: '#a07a12',
    glyph: 'braces',
  },
  {
    kind: 'doc',
    name: 'package.json',
    ext: 'JSON',
    colour: '#a07a12',
    glyph: 'braces',
  },
  {
    kind: 'doc',
    name: 'backup.zip',
    ext: 'ZIP',
    colour: '#c99a1c',
    glyph: 'zip',
  },
  {
    kind: 'doc',
    name: 'assets.7z',
    ext: '7Z',
    colour: '#6f6a5f',
    glyph: 'zip',
  },
  {
    kind: 'doc',
    name: 'demo.mp4',
    ext: 'MP4',
    colour: '#7c4dff',
    glyph: 'play',
  },
  {
    kind: 'doc',
    name: 'voice-memo.m4a',
    ext: 'M4A',
    colour: '#e0457b',
    glyph: 'wave',
  },
  {
    kind: 'doc',
    name: 'mix-final.wav',
    ext: 'WAV',
    colour: '#f06292',
    glyph: 'wave',
  },
  {
    kind: 'doc',
    name: 'Inter.ttf',
    ext: 'TTF',
    colour: '#37474f',
    glyph: 'font',
  },
  {
    kind: 'doc',
    name: 'Serif-Display.otf',
    ext: 'OTF',
    colour: '#8d5524',
    glyph: 'font',
  },
  {
    kind: 'doc',
    name: 'standup.ics',
    ext: 'ICS',
    colour: '#e5534b',
    glyph: 'date',
  },
  {
    kind: 'doc',
    name: 'logo.svg',
    ext: 'SVG',
    colour: '#f08a24',
    glyph: 'shapes',
  },
  {
    kind: 'doc',
    name: 'wireframe.sketch',
    ext: 'SKETCH',
    colour: '#e8a317',
    glyph: 'shapes',
  },
  {
    kind: 'doc',
    name: 'changelog.md',
    ext: 'MD',
    colour: '#3b82c4',
    glyph: 'heading',
  },

  { kind: 'app', name: 'Setup.exe', colour: '#17a1cc', icon: 'package' },
  { kind: 'app', name: 'Terminal.lnk', colour: '#2b2f36', icon: 'terminal' },
  { kind: 'app', name: 'Timer.exe', colour: '#f2994a', icon: 'clock' },
  { kind: 'app', name: 'Paint.lnk', colour: '#c86dd7', icon: 'palette' },

  { kind: 'image', name: 'sunset.jpg', scene: 0 },
  { kind: 'image', name: 'alps-trip.jpg', scene: 1 },
  { kind: 'image', name: 'beach-day.jpg', scene: 2 },
  { kind: 'image', name: 'hike.jpg', scene: 3, shape: 'tall' },
  { kind: 'image', name: 'night-sky.png', scene: 4 },
  { kind: 'image', name: 'headshot.jpg', scene: 5, shape: 'tall' },
  { kind: 'image', name: 'moodboard.png', scene: 6, shape: 'square' },
  { kind: 'image', name: 'dunes.jpg', scene: 7 },
  { kind: 'image', name: 'skyline.mov', scene: 8, video: true },
  { kind: 'image', name: 'gradient.png', scene: 9, shape: 'square' },
  { kind: 'image', name: 'aurora.mp4', scene: 10, video: true },
  { kind: 'image', name: 'leaf-macro.jpg', scene: 11, shape: 'tall' },
  { kind: 'image', name: 'snow-day.heic', scene: 12 },
  { kind: 'image', name: 'pattern.png', scene: 13, shape: 'square' },
  { kind: 'image', name: 'stripes.webp', scene: 14 },
  { kind: 'image', name: 'coral.jpg', scene: 15, shape: 'square' },
  { kind: 'image', name: 'golden-hour.mp4', scene: 0, video: true },
  { kind: 'image', name: 'lake.jpg', scene: 2, shape: 'square' },
  { kind: 'image', name: 'campfire.jpg', scene: 7, shape: 'tall' },
  { kind: 'image', name: 'downtown.jpg', scene: 8, shape: 'tall' },
];
