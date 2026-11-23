import {useEffect, useState} from 'react';
import {invoke} from '@tauri-apps/api/core';

type VmMetrics = {
  running: boolean;
  pid: number | null;
  instance_generation: number;
  sampled_at_ms: number;
  cpu_percent: number | null;
  memory_bytes: number | null;
  network_rx_bytes: number | null;
  network_tx_bytes: number | null;
  network_rx_bytes_per_sec: number | null;
  network_tx_bytes_per_sec: number | null;
  network_available: boolean;
  network_note: string | null;
  cpu_note: string;
  memory_note: string;
};
type Snapshot = {generation: string; samples: VmMetrics[]; current: VmMetrics | null};
const emptySnapshot = (): Snapshot => ({generation: '', samples: [], current: null});
const valid = (value: number | null): value is number => value !== null && Number.isFinite(value) && value >= 0;
function bytes(value: number): string {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {value /= 1024; index++;}
  return `${value.toLocaleString(undefined, {maximumFractionDigits: index ? 1 : 0})} ${units[index]}`;
}
function metric(value: number | null | undefined, format: (n: number) => string, fallback: string): string {
  return value !== undefined && valid(value) ? format(value) : fallback;
}
function Trend({samples, lines, minimum, label, scale}: {
  samples: VmMetrics[];
  lines: {field: 'cpu_percent' | 'memory_bytes' | 'network_rx_bytes_per_sec' | 'network_tx_bytes_per_sec'; color: string}[];
  minimum: number;
  label: string;
  scale: (value: number) => string;
}) {
  const numbers = samples.flatMap(sample => lines.map(line => sample[line.field])).filter(valid);
  const maximum = Math.max(minimum, ...numbers);
  const width = 300, height = 90, padding = 5;
  return <div className="metric-trend">
    <div className="chart-scale"><span>{scale(maximum)}</span><span>0</span></div>
    <svg viewBox={`0 0 ${width} ${height}`} role="img" aria-label={label} preserveAspectRatio="none">
      <title>{label}. Up to 60 real samples; gaps indicate unavailable readings.</title>
      {[padding, height / 2, height - padding].map(y => <line key={y} x1={padding} y1={y} x2={width-padding} y2={y} className="chart-grid"/>)}
      {lines.map(line => {
        let connected = false;
        let path = '';
        const points: {x: number; y: number}[] = [];
        samples.forEach((sample, index) => {
          const value = sample[line.field];
          if (!valid(value)) {connected = false; return;}
          const x = padding + (60 - samples.length + index) / 59 * (width - 2 * padding);
          const y = height - padding - value / maximum * (height - 2 * padding);
          path += `${connected ? 'L' : 'M'}${x.toFixed(2)},${y.toFixed(2)} `;
          points.push({x, y}); connected = true;
        });
        return <g key={line.field}><path d={path} fill="none" stroke={line.color} strokeWidth="2" vectorEffect="non-scaling-stroke"/>{points.length > 0 && <circle cx={points[points.length-1].x} cy={points[points.length-1].y} r="2.5" fill={line.color}/>}</g>;
      })}
    </svg>
    {!numbers.length && <span className="chart-empty">Waiting for measurements</span>}
  </div>;
}
export function ResourceMonitoring({id, running}: {id: string; running: boolean}) {
  const [snapshot, setSnapshot] = useState<Snapshot>(emptySnapshot);
  const [error, setError] = useState('');
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setSnapshot(emptySnapshot()); setError('');
    if (!running) return () => {active = false;};
    const poll = async () => {
      let continuePolling = true;
      try {
        const sample = await invoke<VmMetrics>('get_vm_metrics', {id});
        if (!active) return;
        setError('');
        if (!sample.running) {setSnapshot(emptySnapshot()); continuePolling = false;}
        else setSnapshot(previous => {
          const generation = `${sample.pid}:${sample.instance_generation}`;
          const history = previous.generation === generation ? previous.samples : [];
          return {generation, current: sample, samples: [...history, sample].slice(-60)};
        });
      } catch (cause) {
        if (!active) return;
        setError(String(cause));
        setSnapshot(previous => ({...previous, current: null}));
      } finally {
        // Schedule only after completion: slow native requests never overlap.
        if (active && continuePolling) timer = setTimeout(poll, 2000);
      }
    };
    void poll();
    return () => {active = false; if (timer !== undefined) clearTimeout(timer);};
  }, [id, running]);
  const current = running ? snapshot.current : null;
  const samples = running ? snapshot.samples : [];
  const missing = error ? 'Unavailable' : current ? 'Unavailable' : 'Sampling…';
  const networkMissing = current?.network_available ? 'Sampling…' : current ? 'Unavailable' : missing;
  return <section className="resource-monitor" aria-label="Resource monitoring">
    <div className="resource-heading"><div><div className="eyebrow">RESOURCE MONITORING</div><h2>Live host usage</h2></div><span className={'monitor-state '+(running ? 'live' : '')}>{running ? '● LIVE · EVERY 2 SEC' : 'STOPPED'}</span></div>
    {!running ? <p className="monitor-stopped">Start this machine to monitor CPU, memory and network activity.</p> : <>
      {error && <p className="monitor-error" role="status">Monitoring unavailable: {error}. Retrying automatically.</p>}
      <div className="resource-grid">
        <article className="metric-card"><h3>CPU</h3><div className="metric-value">{metric(current?.cpu_percent, n => `${n.toFixed(1)}%`, !error&&snapshot.samples.length<2?'Sampling…':missing)}</div><Trend samples={samples} lines={[{field:'cpu_percent', color:'#b9f879'}]} minimum={100} scale={n=>`${Math.round(n)}%`} label="QEMU host CPU history"/><p>{current?.cpu_note || 'QEMU host process CPU; 100% is one host core. This can exceed 100%.'}</p></article>
        <article className="metric-card"><h3>Host memory</h3><div className="metric-value">{metric(current?.memory_bytes, bytes, missing)}</div><Trend samples={samples} lines={[{field:'memory_bytes',color:'#a6befb'}]} minimum={1024**3} scale={bytes} label="QEMU host memory footprint history"/><p>{current?.memory_note || 'QEMU host memory footprint. This is not the guest’s used memory.'}</p></article>
        <article className="metric-card"><h3>Network throughput</h3><div className="network-rates"><span><i className="rx-key"/>Receive<strong>{metric(current?.network_rx_bytes_per_sec, n=>`${bytes(n)}/s`, networkMissing)}</strong></span><span><i className="tx-key"/>Send<strong>{metric(current?.network_tx_bytes_per_sec, n=>`${bytes(n)}/s`, networkMissing)}</strong></span></div><Trend samples={samples} lines={[{field:'network_rx_bytes_per_sec',color:'#b9f879'},{field:'network_tx_bytes_per_sec',color:'#71c9e9'}]} minimum={1024} scale={n=>`${bytes(n)}/s`} label="QEMU receive and send rate history"/><div className="network-totals"><span>Received {metric(current?.network_rx_bytes, bytes, networkMissing)}</span><span>Sent {metric(current?.network_tx_bytes, bytes, networkMissing)}</span></div><p>{'Host process TCP/UDP traffic for NAT; not the guest’s full virtual NIC traffic.'}</p>{current?.network_note&&<p>{current.network_note}</p>}</article>
      </div>
      <div className="monitor-caption"><span>Rolling 60 samples · about 2 minutes · resets on restart</span><span>{current ? `PID ${current.pid ?? 'unavailable'} · ${new Date(current.sampled_at_ms).toLocaleTimeString()}` : 'Waiting for a fresh measurement'}</span></div>
    </>}
  </section>;
}
