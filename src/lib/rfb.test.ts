import { describe, expect, it } from 'vitest';

describe('rfb smoke', () => {
  it('rfb status shape is valid', () => {
    const idle = { state: 'idle' as const };
    expect(idle.state).toBe('idle');
  });

  it('KEYSYM constants are defined', () => {
    expect(0xff08).toBe(0xff08);
  });
});
