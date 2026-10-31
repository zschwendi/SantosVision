#!/usr/bin/env python3
"""Package freshly compiled arm64 Mac runtimes with the existing native launcher."""
import argparse,hashlib,json,os,plistlib,re,shutil,subprocess,tempfile,zipfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
def sha(path):
 with path.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
def run(*args):return subprocess.check_output(list(map(str,args)),stderr=subprocess.STDOUT,text=True)
def main():
 p=argparse.ArgumentParser(description=__doc__)
 p.add_argument('--build-dir',type=Path,required=True)
 p.add_argument('--output',type=Path,required=True)
 p.add_argument('--runtime-revision',required=True)
 p.add_argument('--launcher',type=Path,help='Previously compiled launcher from this source revision')
 p.add_argument('--cargo-registry',type=Path,required=True)
 a=p.parse_args()
 if a.output.exists():raise ValueError('Output exists; choose a new directory')
 inputs={}
 for name in ['muguet','muguet-server']:
  binary=a.build_dir/'release'/name
  if run('xcrun','lipo','-archs',binary).strip()!='arm64':raise ValueError('Expected arm64 '+name)
  if 'platform MACOS' not in run('xcrun','vtool','-show-build',binary):raise ValueError('Expected native macOS '+name)
  inputs[name]=sha(binary)
 a.output.parent.mkdir(parents=True,exist_ok=True)
 with tempfile.TemporaryDirectory(prefix='mac-stage-',dir=a.output.parent) as temp:
  stage=Path(temp);app=stage/'V.app';mac=app/'Contents/MacOS';res=app/'Contents/Resources'
  mac.mkdir(parents=True);res.mkdir()
  for name in inputs:shutil.copy2(a.build_dir/'release'/name,mac/name);(mac/name).chmod(0o755)
  if a.launcher:
   shutil.copy2(a.launcher,mac/'Launcher');(mac/'Launcher').chmod(0o755)
  else:
   run('xcrun','swiftc','-O','-parse-as-library','-target','arm64-apple-macos14',ROOT/'launcher/Launcher.swift','-o',mac/'Launcher')
  run('xcrun','actool',ROOT/'assets/muguet.icon','--compile',res,'--app-icon','muguet','--platform','macosx','--minimum-deployment-target','14.0','--output-partial-info-plist',stage/'icon.plist')
  info={'CFBundleName':'V','CFBundleDisplayName':'V','CFBundleIdentifier':'com.zephyrstudios.santosvision.macos','CFBundleExecutable':'Launcher','CFBundleIconFile':'muguet','CFBundleIconName':'muguet','CFBundlePackageType':'APPL','CFBundleShortVersionString':'0.1','CFBundleVersion':'30.1','LSMinimumSystemVersion':'14.0','LSApplicationCategoryType':'public.app-category.games','NSHighResolutionCapable':True,'NSSupportsAutomaticGraphicsSwitching':False,'GCSupportsGameMode':True,'MuguetEmbeddedCode':False,'NSLocalNetworkUsageDescription':'Connect to a multiplayer game server on your network.'}
  (app/'Contents/Info.plist').write_bytes(plistlib.dumps(info))
  shutil.copy2(ROOT/'LICENSE',res/'PortLicense.txt')
  licenses=res/'ThirdPartyLicenses';licenses.mkdir()
  # Keep upstream license/copyright notices for the downloaded Rust dependencies.
  for crate in sorted(a.cargo_registry.glob('*/*')):
   if not (crate/'Cargo.toml').is_file():continue
   for f in sorted(crate.iterdir()):
    if f.is_file() and f.name.upper().startswith(('LICENSE','COPYING','NOTICE','COPYRIGHT')):
     dest=licenses/crate.name/f.name;dest.parent.mkdir(exist_ok=True);shutil.copy2(f,dest)
  for binary in mac.iterdir():
   if run('xcrun','lipo','-archs',binary).strip()!='arm64':raise ValueError('Wrong architecture')
   load=run('xcrun','vtool','-show-build',binary)
   versions=re.findall(r'\bminos (\S+)',load)
   if not versions or any(tuple(map(int,v.split('.')))>(14,0,0) for v in versions):
    raise ValueError('Binary requires a newer OS than the declared macOS14.0 minimum')
   run('codesign','--force','--sign','-','--timestamp=none',binary)
  run('codesign','--force','--sign','-','--timestamp=none',app)
  run('codesign','--verify','--deep','--strict',app)
  a.output.mkdir()
  asset=a.output/'SantosVision-macOS-arm64-30.1-preview.zip'
  files=[]
  with zipfile.ZipFile(asset,'x',zipfile.ZIP_DEFLATED,compresslevel=6,strict_timestamps=False) as z:
   for f in sorted(app.rglob('*')):
    if f.is_symlink():raise ValueError('Unexpected symlink')
    if f.is_file():
     name=f.relative_to(stage).as_posix();z.write(f,name);files.append({'path':name,'bytes':f.stat().st_size,'sha256':sha(f)})
  with zipfile.ZipFile(asset) as z:assert z.testzip() is None
  record={'platform':'macOS','architecture':'arm64','minimumOSVersion':'14.0','build':'30.1','runtimeSourceRevision':a.runtime_revision,'packagingSourceRevision':run('git','-C',ROOT,'rev-parse','HEAD').strip(),'nativeLauncherSourceSHA256':sha(ROOT/'launcher/Launcher.swift'),'inputRuntimeSHA256':inputs,'signing':'ad-hoc; not notarized','gameCodeEmbedded':False,'gameDataIncluded':False,'optionalEasterEggAudioIncluded':False,'launchAndGameplayTested':False,'asset':{'name':asset.name,'bytes':asset.stat().st_size,'sha256':sha(asset)},'files':files}
  (a.output/'manifest.json').write_text(json.dumps(record,indent=2)+'\n')
  (a.output/'SHA256SUMS').write_text(sha(asset)+'  '+asset.name+'\n')
  print(json.dumps({k:v for k,v in record.items() if k!='files'},indent=2))
 for name,h in inputs.items():assert sha(a.build_dir/'release'/name)==h
if __name__=='__main__':main()
