# Changelog

## v0.1.3

- **Linux (experimental):** a Linux version, following nvdweem/PCPanel's approach: the panel through hidraw (with a udev rule), audio through `pactl` (PulseAudio or PipeWire), the tray and settings window through GTK and WebKitGTK. Nearly everything works, including the volume popup, cheat sheet, visualizer and alerts. Tested with a PCPanel Pro on Ubuntu; see the README for setup and limits.
- **Fixed:** a panel label too long for its spot could freeze the settings window.
- **Lights off while you're away:** the panel goes dark while the PC is locked, its screens are off or it's asleep, and lights up again when you're back. On by default; turn it off in *Settings*.
- **New apps start at their dial's level:** when an app starts playing sound, it takes the level of the knob or slider it's mapped to, instead of whatever Windows remembered. On by default; turn it off in *Settings*.
- **Report a problem** (tray menu and the Log tab) opens a GitHub issue with the version, Windows version, panel and recent log already filled in. You see and edit everything before it's posted.
- **SteelSeries Sonar (experimental):** knobs and sliders can set a Sonar channel's level (Master, Game, Chat, Media, Aux, Mic; in streamer mode, the personal mix, the stream mix or both), and buttons can toggle its mute. Built from the API other open-source apps use and not yet tried with Sonar itself, so please report how it works.
- **Safer settings server:** only the settings window the app opens can read or change your settings. Each start makes a new key, so other programs on the PC can't use it.

## v0.1.2

- **Smoother updates:** after updating, the settings window reopens by itself on the new version (keeping any unsaved change), and the old exe is cleaned up automatically. Before, an open settings window could stay stuck on "Downloading".

## v0.1.1

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
