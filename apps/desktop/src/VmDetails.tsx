import React, {useEffect, useRef, useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {ResourceMonitoring} from './ResourceMonitoring';

export const networkModes = [
  {value:'nat',label:'Shared / NAT',description:'Use your Mac’s connection for guest internet access. Incoming connections are blocked by default.'},
  {value:'nat_network',label:'NAT Network',description:'Share a private network and internet access between virtual machines.'},
  {value:'bridged',label:'Bridged',description:'Connect directly to your local network through a selected Mac adapter.'},
  {value:'host_only',label:'Host-only',description:'Connect to your Mac and peers on the macOS host-only network.'},
  {value:'internal',label:'Internal Network',description:'Connect VMs running in this Virmora session. Set static IP addresses inside the guests.'},
  {value:'disconnected',label:'Disconnected',description:'Remove the network adapter.'},
] as const;
export type NetworkMode = typeof networkModes[number]['value'];
export function NetworkModeSettings({mode, bridgeInterface, onBridgeChange, showDescription = true}: {
  mode: NetworkMode;
  bridgeInterface: string;
  onBridgeChange: (name: string) => void;
  showDescription?: boolean;
}) {
  const [interfaces, setInterfaces] = useState<string[]>([]);
  const [lookupError, setLookupError] = useState(false);
  const [check, setCheck] = useState<{key:string; ready:boolean; message:string} | null>(null);
  const [checking, setChecking] = useState(false);
  const checkKey = JSON.stringify([mode, bridgeInterface]);
  async function checkNetwork() {
    setChecking(true);
    setCheck(null);
    try {
      const result = await invoke<{ready:boolean; message:string}>('check_network', {mode, adapter:bridgeInterface});
      setCheck({...result, key:checkKey});
    } catch (error) {
      setCheck({key:checkKey, ready:false, message:String(error)});
    } finally {
      setChecking(false);
    }
  }
  useEffect(() => {
    if (mode !== 'bridged') return;
    let active = true;
    invoke<string[]>('list_bridge_interfaces').then(names => {
      if (active) {setInterfaces(names); setLookupError(false);}
    }).catch(() => {if (active) setLookupError(true);});
    return () => {active = false;};
  }, [mode]);
  const description = networkModes.find(option => option.value === mode)?.description;
  const usesVmnet = mode === 'bridged' || mode === 'nat_network' || mode === 'host_only';
  if (!showDescription && !usesVmnet) return null;
  return <div className="wide">
    {mode === 'bridged' && <label>Mac network adapter
      <select value={bridgeInterface} onChange={event => onBridgeChange(event.target.value)}>
        {[...new Set([bridgeInterface, ...interfaces])].map(name => <option key={name} value={name}>{name}</option>)}
      </select>
      {lookupError && <span className="field-help">Could not list adapters. The saved adapter is shown.</span>}
    </label>}
    {(showDescription || usesVmnet) && <p className="field-help">{showDescription && description}{usesVmnet && ' Requires a running socket_vmnet service or an authorized QEMU build.'}</p>}
    {usesVmnet && <div className="network-check">
      <button className="browse" type="button" disabled={checking} onClick={()=>void checkNetwork()}>
        {checking?'Checking network…':'Check network'}
      </button>
      {check?.key===checkKey && <span role="status">
        {check.ready?'Ready. ':'Unavailable. '}{check.message}
      </span>}
    </div>}
  </div>;
}
export type Config = {
  id:string; name:string; cpus:number; memory_mib:number; disk_size_gib:number;
  guest_os?:'linux'|'windows11'; drivers_iso_path?:string|null; architecture:string; disk_path:string; kernel_path:string; iso_path?:string|null;
  bridge_interface?:string|null; network_mode?:NetworkMode; boot_mode?:'auto'|'installer'|'disk';
};
export type VM = {config:Config; state:'running'|'stopped'|'failed'};
type SettingsUpdate = {name:string; cpus:number; memory_mib:number; network_mode:NetworkMode; bridge_interface:string; iso_path:string|null; drivers_iso_path:string|null};
type Diagnostics = {last_error:string|null; console:string; log:string};

function ConfirmDialog({title, children, onCancel, onConfirm, confirmLabel, busy, destructive, error}: {
  title:string; children:React.ReactNode; onCancel:()=>void; onConfirm:()=>void;
  confirmLabel:string; busy:boolean; destructive?:boolean; error?:string;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {dialog.current?.showModal();}, []);
  return <dialog ref={dialog} className="confirm-dialog" aria-labelledby="confirmation-title" onCancel={event=>{event.preventDefault();if(!busy)onCancel();}}>
    <h2 id="confirmation-title">{title}</h2>{children}{error&&<p className="field-error" role="alert">{error}</p>}
    <div className="dialog-actions"><button className="quiet" autoFocus disabled={busy} onClick={onCancel}>Cancel</button><button className={destructive?'danger':'primary'} disabled={busy} onClick={onConfirm}>{busy?'Working…':confirmLabel}</button></div>
  </dialog>;
}

export function VmDetails({vm, machines, consoleText, refresh}: {
  vm:VM; machines:VM[]; consoleText:string; refresh:()=>Promise<void>;
}) {
  const [busy,setBusy]=useState(false), [operation,setOperation]=useState('');
  const [error,setError]=useState(''), [notice,setNotice]=useState(''), [input,setInput]=useState('');
  const [editing,setEditing]=useState(false), [isoBusy,setIsoBusy]=useState(false);
  const [settings,setSettings]=useState<SettingsUpdate>({name:'',cpus:2,memory_mib:2048,network_mode:'nat',bridge_interface:'en0',iso_path:null,drivers_iso_path:null});
  const [confirm,setConfirm]=useState<'force'|'delete'|null>(null), [removeDisk,setRemoveDisk]=useState(false);
  const [diagnosticsOpen,setDiagnosticsOpen]=useState(false), [diagnostics,setDiagnostics]=useState<Diagnostics|null>(null), [diagnosticError,setDiagnosticError]=useState('');
  const active=useRef(true), diagnosticRequest=useRef(false);
  useEffect(()=>{active.current=true;return()=>{active.current=false;};},[]);
  const loadDiagnostics=async()=>{
    if(diagnosticRequest.current)return;
    diagnosticRequest.current=true;
    try {
      const result=await invoke<Diagnostics>('get_vm_diagnostics',{id:vm.config.id});
      if(active.current){setDiagnostics(result);setDiagnosticError('');if(result.last_error)setDiagnosticsOpen(true);}
    } catch(cause){if(active.current)setDiagnosticError(String(cause));}
    finally{diagnosticRequest.current=false;}
  };
  useEffect(()=>{void loadDiagnostics();},[vm.config.id,vm.state]);
  useEffect(()=>{
    if(!diagnosticsOpen)return;
    const timer=setInterval(()=>void loadDiagnostics(),3000);
    return()=>clearInterval(timer);
  },[diagnosticsOpen,vm.config.id]);
  useEffect(()=>{if(vm.state==='failed'){setDiagnosticsOpen(true);void loadDiagnostics();}},[vm.state]);
  useEffect(()=>{if(vm.state==='running')setEditing(false);},[vm.state]);
  const run=async(command:string,args:Record<string,unknown>={},success='')=>{
    setBusy(true);setOperation(command);setError('');setNotice('');
    try {
      await invoke(command,{id:vm.config.id,...args});
      if(active.current){setNotice(success);setConfirm(null);await refresh();}
      return true;
    } catch(cause){
      if(active.current){setError(String(cause));setDiagnosticsOpen(true);void loadDiagnostics();await refresh().catch(()=>{});}
      return false;
    } finally{if(active.current){setBusy(false);setOperation('');}}
  };
  const beginEdit=()=>{
    setSettings({name:vm.config.name,cpus:vm.config.cpus,memory_mib:vm.config.memory_mib,network_mode:vm.config.network_mode||'nat',bridge_interface:vm.config.bridge_interface||'en0',iso_path:vm.config.iso_path||null,drivers_iso_path:vm.config.drivers_iso_path||null});
    setEditing(true);setError('');setNotice('');
  };
  const chooseIso=async(field: 'iso_path' | 'drivers_iso_path')=>{
    setIsoBusy(true);setError('');
    try{const path=await invoke<string|null>('choose_iso_file');if(path&&active.current)setSettings(old=>({...old,[field]:path}));}
    catch(cause){if(active.current)setError(String(cause));}
    finally{if(active.current)setIsoBusy(false);}
  };
  const name=settings.name.trim();
  const nameError=!name?'Enter a machine name.':machines.some(other=>other.config.id!==vm.config.id&&other.config.name.trim().toLowerCase()===name.toLowerCase())?'A machine with this name already exists.':'';
  const save=async(event:React.FormEvent)=>{
    event.preventDefault();if(nameError)return;
    if(await run('update_vm_settings',{update:{...settings,name}},'Settings saved.'))setEditing(false);
  };
  const bootSource=vm.config.boot_mode==='disk'?'disk':vm.config.boot_mode==='installer'?'installer':vm.config.kernel_path?'kernel':vm.config.iso_path?'installer':'disk';
  const running=vm.state==='running';
  return <>
    <div className="hero"><div><div className="eyebrow">{vm.config.guest_os==='windows11'?'WINDOWS 11 VIRTUAL MACHINE':'LINUX VIRTUAL MACHINE'}</div><h1>{vm.config.name}</h1><p className="subtitle"><span className={'dot '+vm.state}/>{vm.state.charAt(0).toUpperCase()+vm.state.slice(1)} <span className="separator">/</span> QEMU backend</p></div>
      <div className="lifecycle-actions">{running?<><button className="browse" disabled={busy} onClick={()=>void run('restart_vm',{},'Machine restarted.')}>{operation==='restart_vm'?'Restarting…':'Restart'}</button><button className="primary" disabled={busy} onClick={()=>void run('shutdown_vm',{},'Shutdown requested. Waiting for the guest to exit.')}>Shut down</button><button className="danger-outline" disabled={busy} onClick={()=>{setError('');setConfirm('force');}}>Force stop…</button></>:<button className="primary" disabled={busy} onClick={()=>void run('start_vm',{},'Machine started.')}>{operation==='start_vm'?'Starting…':'▶ Start machine'}</button>}</div>
    </div>
    {error&&!confirm&&<div className="error" role="alert">{error}<button aria-label="Dismiss error" onClick={()=>setError('')}>×</button></div>}
    {notice&&<p className="vm-notice" role="status">{notice}</p>}
    {operation==='restart_vm'&&<p className="vm-notice" role="status">Waiting up to 30 seconds for the guest to shut down before restarting.</p>}
    <div className="stats"><div><small>PROCESSOR</small><strong>{vm.config.cpus}<span> cores</span></strong></div><div><small>MEMORY</small><strong>{vm.config.memory_mib/1024}<span> GiB</span></strong></div><div><small>STORAGE</small><strong>{vm.config.disk_size_gib}<span> GiB</span></strong></div><div><small>NETWORK</small><strong className="network-summary">{networkModes.find(mode=>mode.value===(vm.config.network_mode||'nat'))?.label||vm.config.network_mode}</strong></div></div>
    <div className="machine-tools"><button className="browse" disabled={busy||running} onClick={beginEdit}>Edit settings</button><span>{running?'Shut down the machine to change its settings.':'Changes apply the next time this machine starts.'}</span><button className="danger-outline" disabled={busy||vm.state!=='stopped'} onClick={()=>{setError('');setRemoveDisk(false);setConfirm('delete');}}>Delete machine…</button></div>
    {editing&&<section className="settings-editor"><h2>Machine settings</h2><form onSubmit={save}><div className="form-grid"><label className="wide">Name<input required value={settings.name} aria-invalid={!!nameError} onChange={event=>setSettings({...settings,name:event.target.value})}/>{nameError&&<span className="field-error">{nameError}</span>}</label><label>CPU cores<input type="number" min={vm.config.guest_os==='windows11'?2:1} max={vm.config.guest_os==='windows11'?8:64} required value={settings.cpus} onChange={event=>setSettings({...settings,cpus:Number(event.target.value)})}/></label><label>Memory · MiB<input type="number" min={vm.config.guest_os==='windows11'?4096:128} max={1048576} required value={settings.memory_mib} onChange={event=>setSettings({...settings,memory_mib:Number(event.target.value)})}/></label><label className="wide">Network<select value={settings.network_mode} onChange={event=>setSettings({...settings,network_mode:event.target.value as NetworkMode})}>{networkModes.map(mode=><option key={mode.value} value={mode.value}>{mode.label}</option>)}</select></label><NetworkModeSettings mode={settings.network_mode} bridgeInterface={settings.bridge_interface} onBridgeChange={bridge_interface=>setSettings(old=>({...old,bridge_interface}))}/><label className="wide">Installer ISO<span className="file-field"><input readOnly value={settings.iso_path||''} placeholder="No ISO attached"/><button className="browse" type="button" disabled={isoBusy||busy} onClick={()=>void chooseIso('iso_path')}>{isoBusy?'Opening…':'Choose ISO…'}</button>{settings.iso_path&&<button className="browse" type="button" disabled={busy} onClick={()=>setSettings({...settings,iso_path:null})}>Eject</button>}</span><span className="field-help">Ejecting an installer switches installer boot to the installed disk. Choosing a different ISO keeps the current boot mode; select Boot installer below to use it.</span></label>{vm.config.guest_os==='windows11'&&<label className="wide">Windows driver ISO
      <span className="file-field">
        <input readOnly value={settings.drivers_iso_path||''} placeholder="No driver ISO attached"/>
        <button className="browse" type="button" disabled={isoBusy||busy} onClick={()=>void chooseIso('drivers_iso_path')}>Choose driver ISO…</button>
        {settings.drivers_iso_path&&<button className="browse" type="button" disabled={busy} onClick={()=>setSettings({...settings,drivers_iso_path:null})}>Eject</button>}
      </span>
    </label>}</div><div className="editor-actions"><button className="primary" disabled={busy||isoBusy||!!nameError}>{busy?'Saving…':'Save settings'}</button><button type="button" className="quiet" disabled={busy} onClick={()=>setEditing(false)}>Cancel</button></div></form></section>}
    <ResourceMonitoring key={vm.config.id} id={vm.config.id} running={running}/>
    <section className="boot-section"><div><div className="eyebrow">BOOT SOURCE</div><h2>{bootSource==='disk'?'Installed disk':bootSource==='installer'?'Installer ISO':'Linux kernel'}</h2><p className="media-path">{bootSource==='disk'?vm.config.disk_path:bootSource==='installer'?vm.config.iso_path:vm.config.kernel_path}</p>{vm.config.iso_path&&bootSource!=='installer'&&<p className="media-path">Selected ISO: {vm.config.iso_path}</p>}</div>{vm.state==='stopped'&&vm.config.iso_path&&<div className="boot-controls"><button className={'browse '+(bootSource==='installer'?'boot-active':'')} disabled={busy} onClick={()=>void run('set_boot_mode',{mode:'installer'},'Installer boot selected.')}>Boot installer</button><button className={'browse '+(bootSource==='disk'?'boot-active':'')} disabled={busy} onClick={()=>void run('set_boot_mode',{mode:'disk'},'Installed disk boot selected.')}>Boot installed disk</button></div>}<p className="hint">{bootSource!=='kernel'?'The guest opens in a dedicated native VM window with keyboard and mouse support. After installation, shut down the guest and select Boot installed disk. Virmora does not assume installation is complete.':'This machine boots a Linux kernel directly. Use the serial console below to interact with the guest.'}</p>{vm.state==='failed'&&<button className="quiet" disabled={busy} onClick={()=>void run('force_stop_vm',{},'Stopped state reset.')}>Reset stopped state</button>}</section>
    <section className="diagnostics-panel"><button className="diagnostics-toggle" aria-expanded={diagnosticsOpen} onClick={()=>{setDiagnosticsOpen(!diagnosticsOpen);if(!diagnosticsOpen)void loadDiagnostics();}}>{diagnosticsOpen?'▾':'▸'} Logs and diagnostics{diagnostics?.last_error&&<span className="log-error-badge">Last VM error</span>}</button>{diagnosticsOpen&&<div className="diagnostics-body"><div className="diagnostics-toolbar"><span>Last 128 KiB per log · refreshed every 3 seconds</span><button className="browse" onClick={()=>void loadDiagnostics()}>Refresh logs</button></div>{diagnosticError&&<p className="field-error" role="status">{diagnosticError}</p>}{diagnostics?.last_error&&<p className="last-vm-error" role="status">Last VM error: {diagnostics.last_error}</p>}<h3>QEMU process log</h3><pre>{diagnostics?.log||'No QEMU process log available.'}</pre><h3>Serial output</h3><pre>{diagnostics?.console||'No captured serial output available.'}</pre></div>}</section>
    <details className="serial-details"><summary>Interactive serial console · troubleshooting</summary><div className="terminal"><div className="terminal-head"><span><i/> SERIAL CONSOLE</span><span>LIVE OUTPUT · LAST 128 KiB</span></div><pre>{consoleText||'Waiting for guest output…'}</pre><form className="console-input" onSubmit={async event=>{event.preventDefault();try{await invoke('send_console',{id:vm.config.id,text:input});setInput('');}catch(cause){setError(String(cause));}}}><input aria-label="Serial console input" placeholder="Send a line to the guest…" disabled={!running} value={input} onChange={event=>setInput(event.target.value)} maxLength={4096}/><button className="primary" disabled={!running}>Send ↵</button></form></div></details>
    <p className="hint">Persistent disk: {vm.config.disk_path}<br/>Closing Virmora stops its machines.</p>
    {confirm==='force'&&<ConfirmDialog title={`Force stop “${vm.config.name}”?`} busy={busy} error={error} destructive confirmLabel="Force stop" onCancel={()=>setConfirm(null)} onConfirm={()=>void run('force_stop_vm',{},'Machine was forcibly stopped.')}><p>This immediately terminates the VM process. Unsaved guest work may be lost and its filesystem may be damaged. Try Shut down first.</p></ConfirmDialog>}
    {confirm==='delete'&&<ConfirmDialog title={`Delete “${vm.config.name}”?`} busy={busy} error={error} destructive={removeDisk} confirmLabel={removeDisk?'Delete machine and disk':'Remove machine · keep disk'} onCancel={()=>setConfirm(null)} onConfirm={()=>void run('delete_vm',{keepDisk:!removeDisk})}><p>This removes this stopped machine from the Virmora library.</p><p className="deletion-path">{vm.config.disk_path}</p><label className="delete-disk-choice"><input type="checkbox" checked={removeDisk} disabled={busy} onChange={event=>setRemoveDisk(event.target.checked)}/><span>Also permanently delete this machine’s disk, UEFI variables, and TPM state.</span></label><p className="hint">{removeDisk?'The selected disk and firmware state will be permanently deleted. This cannot be undone. The installer ISO is kept.':'Your disk, NVRAM and a recovery configuration stay on disk. The installer ISO is kept.'}</p></ConfirmDialog>}
  </>;
}
