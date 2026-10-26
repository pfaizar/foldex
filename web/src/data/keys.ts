// Keycap labels to the lower-cased KeyboardEvent.key they stand for, so caps on
// the page can react to the visitor's own keyboard.
const ids: Record<string, string> = {
  '↑': 'arrowup',
  '↓': 'arrowdown',
  '←': 'arrowleft',
  '→': 'arrowright',
  '⌫': 'backspace',
  Esc: 'escape',
  Space: ' ',
  Del: 'delete',
  Caps: 'capslock',
};
const aliases: Record<string, string> = {
  control: 'ctrl',
  meta: 'win',
  os: 'win',
};

export const keyId = (label: string) => ids[label] ?? label.toLowerCase();

export const eventKeyId = (event: KeyboardEvent) => {
  const key = event.key.toLowerCase();
  return aliases[key] ?? key;
};

export const modifiers = new Set(['ctrl', 'shift', 'alt', 'win']);
