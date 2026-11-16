# App icon

`assets/icon/` holds the rCAD icon: `rcad.svg` (the "rCAD" sketch, used from 48 px up),
`rcad-small.svg` (just the sketched "r", used at 16-32 px where the word can't be read),
`rcad-<size>.png`, the Windows `rcad.ico`, and `rcad-small-256.png` (the window icon set in main.rs).

To change it, edit `draw.py`, then from `assets/icon/`:

    python3 ../../tools/icon/draw.py
    node ../../tools/icon/export.js rcad.svg . rcad-small.svg   # needs Playwright + Chromium
    python3 ../../tools/icon/ico.py .
    # and rcad-small-256.png: export rcad-small.svg alone into another folder and copy its rcad-256.png
