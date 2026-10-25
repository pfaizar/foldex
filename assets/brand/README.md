# Foldex artwork

`foldex-source.png` is the original artwork supplied by the project maintainer.
`foldex.png` is the transparent runtime icon; `foldex.ico` supplies Windows icon
sizes from 16 to 256 pixels, including exact taskbar sizes at 100–400% display
scaling. `banner.svg` is the repository banner.

The committed exports are ready to build. Regeneration is optional:

```powershell
python -m pip install Pillow
python assets/brand/generate.py
```

The generator crops transparent export margins, preserves the original artwork
and its aspect ratio, and fills the available icon width without an extra inset.
Every ICO frame is resized directly from the source, avoiding repeated scaling.
The banner retains its own spacing. Python and Pillow are not required to build
or run Foldex.

Icon sizes follow [Microsoft's Windows icon construction guidance](https://learn.microsoft.com/en-us/windows/apps/design/iconography/app-icon-construction).
