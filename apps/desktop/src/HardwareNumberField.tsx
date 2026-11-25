import {useId} from 'react';

export function HardwareNumberField({label, value, onChange, min, max, increment = 1}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  min: number;
  max: number;
  increment?: number;
}) {
  const id = useId();
  const number = value === '' ? undefined : Number(value);

  function adjust(delta: number) {
    const next = number === undefined ? min : number + delta;
    onChange(String(Math.min(max, Math.max(min, Math.round(next)))));
  }

  return <div className="hardware-number">
    <label htmlFor={id}>{label}</label>
    <div className="number-control">
      <button type="button" aria-label={`Decrease ${label}`} disabled={number === undefined || number <= min} onClick={() => adjust(-increment)}>−</button>
      <input
        id={id}
        type="number"
        inputMode="numeric"
        min={min}
        max={max}
        step={1}
        required
        value={value}
        onChange={event => onChange(event.currentTarget.value)}
        onBlur={event => {
          const input = event.currentTarget;
          if (input.validity.valid) onChange(String(input.valueAsNumber));
        }}
      />
      <button type="button" aria-label={`Increase ${label}`} disabled={number !== undefined && number >= max} onClick={() => adjust(increment)}>+</button>
    </div>
  </div>;
}
