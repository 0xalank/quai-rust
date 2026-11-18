#!/usr/bin/env python3
"""Build and run the fixed offline transaction pipeline comparison in a fresh stage."""
import argparse,datetime,hashlib,json,os
from pathlib import Path
import shutil,subprocess,tempfile

def main():
    p=argparse.ArgumentParser()
    p.add_argument('--project',default=str(Path(__file__).resolve().parents[1]))
    p.add_argument('--go-quai')
    p.add_argument('--stage')
    a=p.parse_args();root=Path(a.project).resolve();go=Path(a.go_quai).resolve() if a.go_quai else root.parent/'go-quai'
    stage=Path(a.stage).resolve() if a.stage else Path(tempfile.mkdtemp(prefix='quai-tx-pipeline-'))
    stage.mkdir(parents=True,exist_ok=True)
    if any(stage.iterdir()):raise SystemExit('Choose a fresh empty stage directory')
    for name in ['crates']:shutil.copytree(root/name,stage/name)
    for name in ['Cargo.toml','Cargo.lock']:shutil.copy2(root/name,stage/name)
    (stage/'fixtures/protocol').mkdir(parents=True)
    for name in ['transactions.json','transactions-source.json']:shutil.copy2(root/'fixtures/protocol'/name,stage/'fixtures/protocol'/name)
    for name in ['txbench-rust','txbench-go']:shutil.copytree(root/'reference'/name,stage/'reference'/name,ignore=shutil.ignore_patterns('target','__pycache__'))
    (stage/'results').mkdir();(stage/'bin').mkdir()
    sha=lambda path:hashlib.sha256(path.read_bytes()).hexdigest()
    build=json.loads((root/'results/rust-build.json').read_text())
    for name,digest in build['rust_sources'].items():
        if sha(root/name)!=digest:raise SystemExit('Rebuild and verify the shipping source before benchmarking: '+name)
    (stage/'results/shipping-before.json').write_text(json.dumps({'build':build,'binary_hashes':{n:sha(root/'bin'/n) for n in build['binaries']}},indent=2)+'\n')
    subprocess.run(['python3','-u',str(root/'reference/build_tx_pipeline.py'),'--stage',str(stage),'--project',str(root),'--go-quai',str(go)],check=True)
    subprocess.run(['python3','-u',str(root/'reference/run_tx_pipeline.py'),'--stage',str(stage),'--project',str(root)],check=True)
    directory=json.loads((stage/'results/latest.json').read_text())['directory']
    dest=root/'results'/Path(directory).name
    shutil.copytree(stage/directory,dest)
    (dest/'build').mkdir()
    for path in (stage/'results').iterdir():
        if path.is_file():shutil.copy2(path,dest/'build'/path.name)
    shutil.copytree(stage/'reference',dest/'harness/reference')
    shutil.copytree(stage/'crates',dest/'harness/crates')
    for name in ['Cargo.toml','Cargo.lock']:shutil.copy2(stage/name,dest/'harness'/name)
    shutil.copytree(stage/'fixtures',dest/'harness/fixtures')
    shutil.copytree(stage/'bin',dest/'benchmark-binaries')
    (root/'results/latest-transaction-pipeline.json').write_text(json.dumps({'directory':str(dest.relative_to(root))},indent=2)+'\n')
    print('Saved:',dest)
    print('Isolated stage:',stage)
if __name__=='__main__':main()
