// The systems Foldex is built for, in the order the download menus list them.
// A platform without `installer` is shown as coming soon.
import { site } from '../config/site';

export type PlatformId = 'windows' | 'windows-arm' | 'macos' | 'linux';

export interface Platform {
  id: PlatformId;
  name: string;
  detail: string;
  icon: 'windows' | 'apple' | 'linux';
  installer?: { href: string; size: string };
  portable?: { href: string; size: string };
}

export const platforms: Platform[] = [
  {
    id: 'windows',
    name: 'Windows',
    detail: '64-bit (x64)',
    icon: 'windows',
    installer: site.installer,
    portable: site.portable,
  },
  {
    id: 'windows-arm',
    name: 'Windows on Arm',
    detail: 'Snapdragon and other Arm PCs',
    icon: 'windows',
  },
  {
    id: 'macos',
    name: 'macOS',
    detail: 'Apple silicon and Intel',
    icon: 'apple',
  },
  {
    id: 'linux',
    name: 'Linux',
    detail: 'AppImage and .deb',
    icon: 'linux',
  },
];
