// Colour for the soft, grainy backgrounds. Each palette runs from its deepest
// tone to its highlight: base, shadow, mid, light. Keep them low in saturation;
// two neighbouring hues at most, so the blends never turn muddy.
export const auras = {
  dusk: ['#1c2024', '#3b444d', '#5d7183', '#dcb894'],
  slate: ['#262c33', '#46525e', '#8193a3', '#e3e6e5'],
  moss: ['#1b2321', '#2f3d39', '#5c7a70', '#d5dccd'],
  clay: ['#28211e', '#4a403b', '#9a6c55', '#ead9c6'],
  fog: ['#8c969c', '#a9b1b5', '#cfd3d2', '#f6f2ea'],
  sand: ['#a89a88', '#c3b6a5', '#e2d9cc', '#fbf8f2'],
} as const satisfies Record<string, readonly [string, string, string, string]>;

export type AuraName = keyof typeof auras;
