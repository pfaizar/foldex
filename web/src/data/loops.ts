// Key presses captured while recording each loop (seconds from the start of
// the encoded video). Generated from the recording log; used by the keycast.
export type LoopName = 'palette' | 'search' | 'navigate';

export const loops: Record<
  LoopName,
  { duration: number; keys: [number, string][] }
> = {
  palette: {
    duration: 9.48,
    keys: [
      [0.8, '^+p'],
      [1.4, 's'],
      [1.56, 'p'],
      [1.76, 'l'],
      [1.92, 'i'],
      [2.12, 't'],
      [2.96, '{ENTER}'],
      [5.0, '^+p'],
      [5.6, 'c'],
      [5.72, 'l'],
      [5.84, 'o'],
      [6.0, 's'],
      [6.12, 'e'],
      [6.28, ' '],
      [6.4, 'p'],
      [6.6, 'a'],
      [6.72, 'n'],
      [6.88, 'e'],
      [7.04, 'l'],
      [7.92, '{ENTER}'],
    ],
  },
  search: {
    duration: 7.2,
    keys: [
      [0.8, '^f'],
      [1.28, 'p'],
      [1.44, 'n'],
      [1.6, 'g'],
      [2.48, '^e'],
      [5.12, '{ESC}'],
      [5.44, '{ESC}'],
      [5.72, '{ESC}'],
    ],
  },
  navigate: {
    duration: 6.64,
    keys: [
      [0.56, '{DOWN}'],
      [1.24, '{ENTER}'],
      [2.28, '{DOWN}'],
      [2.92, '{ENTER}'],
      [4.16, '%{UP}'],
      [5.12, '%{UP}'],
    ],
  },
};
