#!/usr/bin/env python3
"""Freeze upstream license texts for the pinned Rust dependency graph."""
import argparse,json,os,subprocess,re,hashlib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser();p.add_argument('--write',action='store_true');a=p.parse_args()
target=subprocess.check_output(['rustc','-vV'],text=True).split('host: ')[1].splitlines()[0]
metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--offline','--all-features','--format-version','1','--filter-platform',target],cwd=ROOT,text=True))
tree=subprocess.check_output(['cargo','tree','--locked','--offline','--workspace','--all-features','--edges','normal,build,dev','--target',target,'--prefix','none','--format','{p}'],cwd=ROOT,text=True)
selected={tuple(m.groups())for line in tree.splitlines()if(m:=re.match(r'^(\S+) v(\S+)',line))}
external=ROOT/'licenses/upstream/sources.json';external=json.loads(external.read_text())if external.exists()else{}
parts=['Third-party Rust dependency licenses\n\nPinned native and optional private-transport dependencies. Original copyright and attribution text follows.\n']
missing=[];count=0
for package in sorted(metadata['packages'],key=lambda p:(p['name'],p['version'])):
 if package['source'] is None or(package['name'],package['version'])not in selected:continue
 directory=Path(package['manifest_path']).parent
 candidates={f for pattern in ['LICENSE*','LICENCE*','COPYING*','NOTICE*','UNLICENSE*','license*']for f in directory.glob(pattern)if f.is_file()}
 if package.get('license_file'):candidates.add(directory/package['license_file'])
 if not candidates:
  item=external.get(package['name']+' '+package['version'])
  if item:
   source=ROOT/item['file'];assert hashlib.sha256(source.read_bytes()).hexdigest()==item['sha256'];candidates.add(source)
 if not candidates:missing.append(package['name']+' '+package['version']);continue
 count+=1;parts.append('\n'+'='*72+'\n'+package['name']+' '+package['version']+'\nLicense expression: '+str(package.get('license'))+'\n')
 for source in sorted(candidates):parts.append('\n'+source.name+'\n\n'+source.read_text(errors='strict')+'\n')
if missing:raise SystemExit('Missing upstream license text: '+', '.join(missing))
output=('\n'.join(parts)).encode();path=ROOT/'THIRD_PARTY_LICENSES.txt'
if a.write:path.write_bytes(output)
elif path.read_bytes()!=output:raise SystemExit('Third-party license bundle differs')
print(json.dumps({'passed':True,'packages':count,'bytes':len(output),'target':target,'includes_optional_private_transport':True}))
