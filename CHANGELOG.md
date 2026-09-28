# Changelog

## Unreleased

- **Elgato Wave Link 3 (experimental):** knobs and sliders can control a Wave Link channel, a channel in one mix, or a whole mix, and buttons can toggle their mute. It's built from the protocol other open-source apps use and hasn't been tried with Wave Link itself yet, so please report how it works. Wave Link 2 isn't supported.

## v0.1.0

The first release of PCPanel Revive: a free, open-source replacement for the official PCPanel software on Windows 10/11, for the PCPanel Pro, the PCPanel Mini and the original wooden PCPanel. One small exe in the tray; no Java, no services, no installer.

**Getting started:** download `pcpanel-revive.exe`, quit the official PCPanel software, and run it. If you used the official software, one click imports your profiles; if you're new, Quick setup builds a ready-made layout for the apps on your PC.

> **"Windows protected your PC"?** The exe isn't code-signed yet, so SmartScreen may warn you the first time. Click **More info > Run anyway**.

### What it does

- **A live picture of your panel.** Move a control and its settings open. Lights, positions, app icons and live volumes are shown as they really are.
- **Volume for anything:** apps (with real icons), devices, the app in front, "everything else", OBS sources and Voicemeeter. No volume jumps: after a volume changes elsewhere, the dial takes over smoothly.
- **Knobs beyond volume:** scroll, zoom, brush size or timeline steps, one keystroke per step; or dim your Home Assistant or Philips Hue lights.
- **Buttons:** press, double press and hold on every knob, with 29 kinds of action: media keys and shortcuts, mute, audio devices, focus mode, open apps and websites, type text, turn displays off, lock the PC, OBS, web requests and more.
- **Shift layer and profile slider:** hold a knob to give every control a second job, or turn a slider into a profile switch with a color per profile.
- **Cheat sheet:** hold a knob to see a map of your panel with what every control does.
- **Lighting:** per-control colors, fills and meters, 12 presets plus your own, mute colors, and a music visualizer that dances to what's playing (only for the apps and lights you choose).
- **Notification lights:** pulse a knob when Discord has an unread message, blink on an Outlook notification, or light the logo red while any app is using your microphone.
- **Profiles** that switch automatically with the app in front.
- **Safe to tinker:** autosave, undo/redo, automatic backups, a panel self-test, and updates from GitHub with one click.

Tested on a PCPanel Pro. The Mini and the original are supported with the same protocol as the official software but haven't been tested on real hardware yet; reports are welcome.
