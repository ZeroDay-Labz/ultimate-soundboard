Ultimate Soundboard for Windows
================================

This is a portable build: ultimate-soundboard.exe runs directly, no
installer, no admin rights needed.

Audio output device
--------------------
Open Settings inside the app to pick a specific output device -- this is
how you route the soundboard into a Voicemeeter virtual input (or any
other virtual cable) so Discord/OBS/a softphone can pick it up as a
microphone, the same way you'd route any other program into Voicemeeter.

Opus / Discord-style .ogg files
--------------------------------
Most formats (wav, mp3, flac, aac, ogg-vorbis) decode natively with no
extra setup. Opus decoding (including Discord-style .ogg/.opus files)
falls back to ffmpeg. If you hit a file that won't play:

  1. Install ffmpeg (https://ffmpeg.org/download.html#build-windows) and
     make sure ffmpeg.exe is on your PATH, OR
  2. Drop ffmpeg.exe in the same folder as ultimate-soundboard.exe, OR
  3. Set the ULTIMATE_SOUNDBOARD_FFMPEG environment variable to the full
     path of your ffmpeg.exe.

Global hotkeys
--------------
Space (and any per-button hotkeys you set) work system-wide on Windows --
they'll stop/trigger sounds even while another app has focus.
