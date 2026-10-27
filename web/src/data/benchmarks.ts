// Benchmark figures shown on the landing page.

export interface Benchmark {
  title: string;
  detail: string;
  unit: 'ms' | 's' | 'MB';
  /** Completes "N× …" in the headline figure. */
  claim: string;
  /** Foldex first; every other entry is compared against it. */
  results: { app: string; value: number }[];
  /**
   * What the race chart draws: `progress` is how much of the task each app has
   * done over time; `level` is a reading over a session (`span` minutes).
   */
  curve:
    | { kind: 'progress'; label: string }
    | { kind: 'level'; label: string; span: number };
}

export const benchmarkInfo = {
  machine: 'Test machine to be announced',
  method:
    'Median of 10 runs on Windows 11, warm disk cache unless noted. Lower is better.',
};

export const benchmarks: Benchmark[] = [
  {
    title: 'Cold start',
    claim: 'faster to start',
    detail: 'Launch to first usable window',
    unit: 'ms',
    results: [
      { app: 'Foldex', value: 210 },
      { app: 'File Explorer', value: 740 },
    ],
    curve: { kind: 'progress', label: 'Window ready' },
  },
  {
    title: 'Open a large folder',
    claim: 'faster to open',
    detail: '100,000 files, sorted by name',
    unit: 'ms',
    results: [
      { app: 'Foldex', value: 380 },
      { app: 'File Explorer', value: 4200 },
    ],
    curve: { kind: 'progress', label: 'Items listed' },
  },
  {
    title: 'Search subfolders',
    claim: 'faster to search',
    detail: 'Filename match across 250,000 items',
    unit: 's',
    results: [
      { app: 'Foldex', value: 0.6 },
      { app: 'File Explorer', value: 18.5 },
    ],
    curve: { kind: 'progress', label: 'Items searched' },
  },
  {
    title: 'Memory at idle',
    claim: 'less memory',
    detail: 'Private working set, one window',
    unit: 'MB',
    results: [
      { app: 'Foldex', value: 46 },
      { app: 'File Explorer', value: 118 },
    ],
    curve: { kind: 'level', label: 'Memory over a session', span: 10 },
  },
];
