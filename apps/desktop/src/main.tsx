import React, {useEffect,useRef,useState} from 'react';
import {createRoot} from 'react-dom/client';
import {invoke} from '@tauri-apps/api/core';
import './style.css';
import {VmDetails, NetworkModeSettings, networkModes, type NetworkMode, type VM} from './VmDetails';
type DesktopSettings={vm_directory:string};
function App(){
 const [vms,setVms]=useState<VM[]>([]),[selected,setSelected]=useState(''),[consoleText,setConsole]=useState(''),[error,setError]=useState(''),[busy,setBusy]=useState(false),[creating,setCreating]=useState(false);
 const [settings,setSettings]=useState<DesktopSettings|null>(null);
 const [isoBusy,setIsoBusy]=useState(false);
 const [folder,setFolder]=useState<string|null>(null),[folderBusy,setFolderBusy]=useState(false);
 const [form,setForm]=useState({name:'linux machine',cpus:2,memory_mib:2048,disk_size_gib:16,iso_path:'',drivers_iso_path:'',guest_os:'linux' as 'linux'|'windows11',architecture:'aarch64',network_mode:'nat' as NetworkMode,bridge_interface:'en0'});
 const trimmedName=form.name.trim();
 const nameError=!trimmedName?'Enter a machine name.':vms.some(vm=>vm.config.name.trim().toLowerCase()===trimmedName.toLowerCase())?`A machine named “${trimmedName}” already exists. Choose a different name.`:'';
 const refreshSequence=useRef(0);
 const refresh=async()=>{
  const request=++refreshSequence.current;
  const list=await invoke<VM[]>('list_vms');
  if(request!==refreshSequence.current)return;
  setVms(list);
  setSelected(old=>list.some(vm=>vm.config.id===old)?old:list[0]?.config.id||'');
 };
 useEffect(()=>{invoke<DesktopSettings>('get_desktop_settings').then(setSettings).catch(e=>setError(String(e)));},[]);
 useEffect(()=>{refresh().catch(e=>setError(String(e)));const timer=setInterval(()=>refresh().catch(e=>setError(String(e))),2000);return()=>clearInterval(timer);},[]);
 useEffect(()=>{let active=true;const read=()=>selected&&invoke<string>('read_console',{id:selected}).then(s=>{if(active)setConsole(s)}).catch(()=>{if(active)setConsole('')});setConsole('');read();const timer=setInterval(read,1000);return()=>{active=false;clearInterval(timer)}},[selected]);
 const create=async(e:React.FormEvent)=>{e.preventDefault();if(nameError){setError(nameError);return;}setBusy(true);setError('');try{const created=await invoke<VM>('create_vm',{request:{...form,name:trimmedName,drivers_iso_path:form.drivers_iso_path||null,iso_path:form.iso_path||null,vm_directory:folder}});setSelected(created.config.id);setCreating(false);await refresh()}catch(e){setError(String(e))}finally{setBusy(false)}};
 const chooseFolder=async()=>{setFolderBusy(true);setError('');try{const path=await invoke<string|null>('choose_vm_folder');if(path)setFolder(path);}catch(e){setError(String(e))}finally{setFolderBusy(false)}};
 const chooseIso=async()=>{setIsoBusy(true);setError('');try{const path=await invoke<string|null>('choose_iso_file');if(path)setForm(old=>({...old,iso_path:path}));}catch(e){setError(String(e))}finally{setIsoBusy(false)}};
 const windows=form.guest_os==='windows11';
 const chooseDrivers=async()=>{
  setIsoBusy(true);setError('');
  try {
   const path=await invoke<string|null>('choose_iso_file');
   if(path)setForm(old=>({...old,drivers_iso_path:path}));
  } catch(error){setError(String(error));}
  finally{setIsoBusy(false);}
 };
 const vm=vms.find(v=>v.config.id===selected);
 return <div className="shell"><aside><div className="brand"><span className="mark">V</span>Virmora</div><div className="section">YOUR MACHINES <span>{vms.length}</span></div><nav>{vms.map(v=><button key={v.config.id} className={'machine '+(v.config.id===selected?'selected':'')} onClick={()=>{setSelected(v.config.id);setCreating(false)}}><span className={'dot '+v.state}/><span>{v.config.name}<small>{v.config.architecture==='aarch64'?'ARM64':'Intel x86_64'} · {v.config.guest_os==='windows11'?'Windows 11':'Linux'}</small></span></button>)}</nav><button className="new" onClick={()=>setCreating(true)}>＋ New virtual machine</button><div className="aside-foot">LOCAL FIRST<br/><span>Your machines. Your Mac.</span></div></aside><main><header><div className="eyebrow">VIRMORA / WORKSPACE</div></header>{error&&<div role="alert" className="error">{error}<button onClick={()=>setError('')}>×</button></div>}
 {creating?<><h1>A new place to build.</h1><p className="subtitle">Choose a name, ISO image and hardware for your virtual machine.</p><form onSubmit={create}><div className="form-grid"><label className="wide">Name<input required aria-invalid={!!nameError} aria-describedby={nameError?'machine-name-error':undefined} value={form.name} onChange={e=>setForm({...form,name:e.target.value})}/>{nameError&&<span id="machine-name-error" className="field-error" role="status">{nameError}</span>}</label><div className="wide folder-field"><span id="folder-label">Folder</span><button type="button" className="folder-select" aria-labelledby="folder-label folder-value" aria-haspopup="dialog" disabled={folderBusy} onClick={chooseFolder}><span id="folder-value">{folderBusy?'Choosing folder…':folder||settings?.vm_directory||'Select folder…'}</span><span aria-hidden="true">▾</span></button></div><label className="wide">Guest operating system<select value={form.guest_os} onChange={event=>{
  const guest_os=event.target.value as 'linux'|'windows11';
  setForm(old=>guest_os==='windows11'
    ? {...old,guest_os,name:old.name==='linux machine'?'Windows 11':old.name,architecture:'aarch64',cpus:Math.min(8,Math.max(2,old.cpus)),memory_mib:Math.max(4096,old.memory_mib),disk_size_gib:Math.max(64,old.disk_size_gib)}
    : {...old,guest_os,drivers_iso_path:''});
 }}><option value="linux">Linux</option><option value="windows11">Windows 11 ARM64 (preview)</option></select></label><label className="wide">ISO Image<span className="file-field"><input aria-label="Selected ISO Image" readOnly value={form.iso_path} placeholder="&lt;not selected&gt;"/><button type="button" className="browse" disabled={isoBusy} onClick={chooseIso}>{isoBusy?'Opening…':'Choose ISO…'}</button>{form.iso_path&&<button type="button" className="browse" onClick={()=>setForm(old=>({...old,iso_path:''}))}>Clear</button>}</span><span className="field-help">Select an operating system installer (.iso). Use an ISO matching the selected architecture. Starting this machine opens the installer in a dedicated VM display window.</span></label>{windows&&<label className="wide">Windows driver ISO · optional
 <span className="file-field"><input readOnly value={form.drivers_iso_path} placeholder="No driver ISO selected"/><button type="button" className="browse" disabled={isoBusy} onClick={()=>void chooseDrivers()}>Choose drivers…</button>{form.drivers_iso_path&&<button type="button" className="browse" onClick={()=>setForm(old=>({...old,drivers_iso_path:''}))}>Clear</button>}</span>
 <span className="field-help">Use a driver ISO containing signed ARM64 VirtIO network drivers. Windows Setup may ask you to load them. On the Mac, install swtpm and run scripts/setup-windows-firmware.sh before starting.</span>
 </label>}<label>Architecture<select disabled={windows} value={form.architecture} onChange={e=>setForm({...form,architecture:e.target.value})}><option value="aarch64">ARM64 / Apple Silicon</option><option value="x86_64">x86_64 / Intel</option></select></label>{(['cpus','memory_mib','disk_size_gib'] as const).map(key=><label key={key}>{{cpus:'CPU cores',memory_mib:'Memory · MiB',disk_size_gib:'Persistent disk · GiB'}[key]}<input type="number" min={windows?{cpus:2,memory_mib:4096,disk_size_gib:64}[key]:key==='memory_mib'?128:1} max={key==='cpus'?(windows?8:64):key==='memory_mib'?1048576:16384} value={form[key]} onChange={e=>setForm({...form,[key]:Number(e.target.value)})}/></label>)}</div><fieldset className="network-section"><legend>Network</legend><p className="field-help">Choose how this machine connects. Your selection is saved with the VM.</p><div className="network-options">{networkModes.map(mode=><label key={mode.value} className={'network-option '+(form.network_mode===mode.value?'active':'')}><input type="radio" name="network_mode" value={mode.value} checked={form.network_mode===mode.value} onChange={()=>setForm({...form,network_mode:mode.value})}/><span><strong>{mode.label}</strong><small>{mode.description}</small></span></label>)}</div><NetworkModeSettings showDescription={false} mode={form.network_mode} bridgeInterface={form.bridge_interface} onBridgeChange={bridge_interface=>setForm(old=>({...old,bridge_interface}))}/></fieldset><p className="hint">Creates a persistent sparse raw disk in {folder||settings?.vm_directory||'the configured VM directory'}. The selected network mode and serial output are configured automatically. The installer ISO selection is saved with the machine.</p><button className="primary" disabled={busy||!form.iso_path||!!nameError}>Create machine</button><button type="button" className="quiet" onClick={()=>setCreating(false)}>Cancel</button></form></>:vm?<VmDetails key={vm.config.id} vm={vm} machines={vms} consoleText={consoleText} refresh={refresh}/>:<div className="empty"><span className="large-mark">V</span><h1>Your next workspace<br/>starts here.</h1><p className="subtitle">Create a Linux or Windows virtual machine with persistent storage,<br/>NAT networking and a dedicated VM display.</p><button className="primary" onClick={()=>setCreating(true)}>＋ Create your first machine</button><p className="hint">VM files: {settings?.vm_directory||'Loading storage location…'}</p></div>}</main></div>
}
createRoot(document.getElementById('root')!).render(<App/>);
