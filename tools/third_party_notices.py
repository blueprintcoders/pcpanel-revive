"""Write THIRD-PARTY-NOTICES.txt: every library compiled into the Windows exe, with its license text.

Run from the repo root after changing dependencies:  python tools/third_party_notices.py
"""
import glob
import os
import subprocess

TARGET = 'x86_64-pc-windows-msvc'
tree = subprocess.run(
    ['cargo', 'tree', '-e', 'normal,no-proc-macro', '--target', TARGET, '--prefix', 'none', '-f', '{p}|{l}|{r}'],
    capture_output=True, text=True, check=True).stdout

crates = {}
for line in tree.splitlines():
    spec, lic, repo = (line.split('|') + ['', ''])[:3]
    name, version = spec.split()[:2]
    if name == 'pcpanel-revive':
        continue
    crates[(name, version.lstrip('v'))] = (lic.replace(' (*)', '').strip(), repo.replace(' (*)', '').strip())

registry = glob.glob(os.path.expanduser('~/.cargo/registry/src/*'))


def license_files(name, version):
    for reg in registry:
        d = os.path.join(reg, f'{name}-{version}')
        if os.path.isdir(d):
            found = sorted(f for f in os.listdir(d)
                           if f.upper().startswith(('LICENSE', 'LICENCE', 'COPYING', 'UNLICENSE', 'NOTICE')) and os.path.isfile(os.path.join(d, f)))
            return [open(os.path.join(d, f), encoding='utf-8', errors='replace').read().strip() for f in found]
    return []


# Group identical license texts so each is printed once, with the crates that use it.
texts = {}
missing = []
for (name, version), (lic, repo) in sorted(crates.items()):
    files = license_files(name, version)
    if not files:
        missing.append(f'{name} {version}')
    for t in files:
        texts.setdefault(t, []).append(f'{name} {version}')

out = ['Third-party software in PCPanel Revive',
       '=' * 38,
       '',
       'PCPanel Revive is licensed under the GNU General Public License v3.0 or later (see LICENSE).',
       'The Windows exe includes the following open-source libraries, under their own licenses.',
       'The settings window is shown by Microsoft Edge WebView2, which comes with Windows and is not included in the exe.',
       '',
       'Libraries',
       '---------',
       '']
width = max(len(f'{n} {v}') for n, v in crates)
for (name, version), (lic, repo) in sorted(crates.items()):
    out.append(f'{name + " " + version:<{width}}  {lic:<40}  {repo}'.rstrip())
out += ['', 'License texts', '-------------', '']
for text, users in sorted(texts.items(), key=lambda kv: kv[1][0]):
    out.append('Used by: ' + ', '.join(users))
    out.append('')
    out.append(text)
    out.append('')
    out.append('-' * 78)
    out.append('')
if missing:
    out += ['These libraries ship no license file; their licenses are listed above:', ', '.join(missing), '']
open('THIRD-PARTY-NOTICES.txt', 'w', encoding='utf-8', newline='\n').write('\n'.join(out))
print(f'{len(crates)} libraries, {len(texts)} distinct license texts, {len(missing)} without a license file')
