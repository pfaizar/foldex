# One block per website loop. This is the file to edit.
#
# Each scene runs against a freshly staged, fictional demo workspace. Steps are
# ordinary PowerShell: Wait, Send (SendKeys chord) and Type (literal text) cover
# most features, and any other command can be mixed in.
#
# Key fields:
#   Name        file name for the take and the loop on the website
#   Crop        ffmpeg crop after capture, "w:h:x:y"; omit to keep the window
#   Width       output width in pixels; height follows the aspect ratio
#   Fade        seconds crossfaded from the start into the end (0 disables)
#   Publish     copy into web/public/media and update web/src/data/loops.ts
#   Theme       dark or light demo theme
#   SinglePane  close the right pane in the first tab before recording
#   Setup       runs before capture starts (e.g. open a dialog you do not want recorded)
#   Steps       runs while recording; every Send is logged for the keycast
#   After       runs after capture stops
#   Capture     fixed output size for windows that resize, @{ Width; Height; Fill }

@(
    @{
        Name       = 'palette'
        Note       = 'Open the command palette, run Split, then close the panel.'
        Theme      = 'dark'
        SinglePane = $true
        Crop       = '1280:768:0:0'
        Width      = 1280
        Fade       = 0.4
        Publish    = $true
        Steps      = {
            Wait 1200
            Send '^+p'; Wait 500
            Type 'split' 130; Wait 700
            Send '{ENTER}'; Wait 2000
            Send '^+p'; Wait 500
            Type 'close panel' 110; Wait 700
            Send '{ENTER}'; Wait 1500
        }
    }

    @{
        Name    = 'search'
        Note    = 'Search the demo folder for PNGs, then clear and close.'
        Theme   = 'dark'
        Crop    = '652:772:0:0'
        Width   = 652
        Fade    = 0.4
        Publish = $true
        Steps   = {
            Wait 1200
            Send '^f'; Wait 350
            Type 'png' 140; Wait 700
            Send '^e'; Wait 2600
            Send '{ESC}'; Wait 250
            Send '{ESC}'; Wait 250
            Send '{ESC}'; Wait 1400
        }
    }

    @{
        Name       = 'navigate'
        Note       = 'Move through folders with the keyboard and go back up.'
        Theme      = 'dark'
        SinglePane = $true
        Crop       = '900:430:0:0'
        Width      = 900
        Fade       = 0.4
        Publish    = $true
        Steps      = {
            Wait 1000
            Send '{DOWN}'; Wait 600
            Send '{ENTER}'; Wait 1000
            Send '{DOWN}'; Wait 600
            Send '{ENTER}'; Wait 1200
            Send '%{UP}'; Wait 900
            Send '%{UP}'; Wait 1400
        }
    }
)
