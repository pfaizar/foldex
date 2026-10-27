// Every release, newest first, in plain words. The full notes live in
// docs/releases; add a release here when it ships.
export type ChangeKind = 'new' | 'improved' | 'fixed';

export interface Release {
  version: string;
  date: string;
  title: string;
  summary: string;
  changes: { kind: ChangeKind; text: string }[];
}

export const releases: Release[] = [
  {
    version: '0.1.4',
    date: '2026-10-26',
    title: 'Faster search, bigger folders',
    summary:
      'Search keeps up as files change, huge folders open in full, and the search bar is tidier.',
    changes: [
      {
        kind: 'improved',
        text: 'Search results stay live. Folders being searched share one live catalog, long result lists scroll in smoothly, and a folder that is deleted and made again is picked up automatically.',
      },
      {
        kind: 'new',
        text: 'Open a very large folder in full with Load all items. Dates and cloud status fill in without holding up the list.',
      },
      {
        kind: 'new',
        text: 'Search conditions open from a filter button inside the search bar, and a combined search can be saved as a collection.',
      },
      {
        kind: 'new',
        text: 'Launcher shortcuts that use the Windows key, such as Win+Space, now work.',
      },
      {
        kind: 'improved',
        text: 'File actions run independently of each other, and screen readers can read the rows of a file list.',
      },
      {
        kind: 'fixed',
        text: 'Recording a shortcut no longer stops working after settings refresh.',
      },
      {
        kind: 'improved',
        text: 'Several dialogs lost text that repeated what they already said.',
      },
    ],
  },
  {
    version: '0.1.3',
    date: '2026-10-25',
    title: 'Drag and drop, done properly',
    summary:
      'Drag files anywhere they make sense, see what you are carrying, and stop seeing progress windows flash.',
    changes: [
      {
        kind: 'new',
        text: 'Drag files and folders onto folders, split panes, the path bar and the sidebar, or to and from File Explorer.',
      },
      {
        kind: 'new',
        text: 'A see-through preview follows the pointer with a count of the items, and the place you can drop on lights up.',
      },
      {
        kind: 'improved',
        text: 'Quick copies, moves, renames and deletes no longer flash a progress window. Longer ones show Windows progress after a second.',
      },
      {
        kind: 'fixed',
        text: 'Shortcut recording catches Win and Alt combinations before Windows menus can take them, and Escape always ends it safely.',
      },
    ],
  },
  {
    version: '0.1.2',
    date: '2026-10-25',
    title: 'Shortcuts, all in one place',
    summary:
      'Every shortcut lives in Hotkeys now, and recording them is simpler.',
    changes: [
      {
        kind: 'improved',
        text: 'All shortcuts, including the one that opens the launcher, are set in Hotkeys. Your launcher shortcut carries over.',
      },
      {
        kind: 'new',
        text: 'Record full combinations such as Win+Ctrl+Shift+Alt+A, with every key kept.',
      },
      {
        kind: 'new',
        text: 'Record a sequence of combinations of any length, then choose Assign.',
      },
      {
        kind: 'fixed',
        text: 'If Windows refuses a new shortcut, the one that worked before is kept.',
      },
    ],
  },
  {
    version: '0.1.1',
    date: '2026-10-25',
    title: 'An installer, and updates',
    summary: 'Install Foldex like any other app, and let it update itself.',
    changes: [
      {
        kind: 'new',
        text: 'A Windows installer: pick a folder, add a desktop shortcut if you like, no admin password needed.',
      },
      {
        kind: 'new',
        text: 'Check for, download and install new versions from Options, then Application Updates.',
      },
      {
        kind: 'improved',
        text: 'Your settings and themes are kept in your Windows profile, so they survive updates and even uninstalling.',
      },
      {
        kind: 'improved',
        text: 'The Visual C++ runtime is built in; there is nothing extra to install.',
      },
    ],
  },
  {
    version: '0.1.0',
    date: '2026-10-25',
    title: 'Hello, Foldex',
    summary: 'The first public preview of Foldex.',
    changes: [
      {
        kind: 'new',
        text: 'Browse in tabs and split panes, search by name and choose how each folder looks.',
      },
      {
        kind: 'new',
        text: 'Open the launcher from anywhere with Ctrl+Alt+Space.',
      },
      {
        kind: 'new',
        text: 'Copy, move, delete and right-click the way Windows does, Recycle Bin included.',
      },
      {
        kind: 'new',
        text: 'Dark and light themes, your own shortcuts, and a sidebar you arrange.',
      },
    ],
  },
];
