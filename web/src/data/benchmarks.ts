// Benchmark figures shown on the landing page.
// While `placeholder` is true the section is labelled as sample data.
// Replace the values with measured results, then set `placeholder` to false.

export interface Benchmark {
  title: string;
  detail: string;
  unit: string;
  /** Completes "N× …" in the headline figure. */
  claim: string;
  /** Foldex first; every other entry is compared against it. */
  results: { app: string; value: number }[];
}

export const benchmarkInfo = {
  placeholder: true,
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
  },
];
