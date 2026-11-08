"""Release requirement for repeated native Japanese IME cancellation."""
import re


def require_repeated_ime(proof, count=8):
    cycles = proof['uia']['native_ime']['cycles']
    screenshots = proof['ime_cycle_screenshots']
    if len(cycles) != count or len(screenshots) != count:
        raise ValueError('Incomplete native IME cycle evidence')
    for index, (cycle, shot) in enumerate(zip(cycles, screenshots), 1):
        trace = cycle['cancel_trace']
        if not (
            cycle['cancel_start_value'] == 'とりけし'
            and cycle['cancelled_roman_keys'] == 'torikeshi'
            and cycle['committed_roman_keys'] == 'genbun'
            and cycle['committed_value'] == '原文'
            and cycle['unicode_injection_used'] is False
            and cycle['cancel_escape_presses'] == 2
            and [step['escape'] for step in trace] == [1, 2]
            and trace[-1]['value'] == ''
            and shot['cycle'] == index
            and re.fullmatch(r'[0-9a-f]{64}', shot['sha256'])
        ):
            raise ValueError(f'Native IME cancellation/conversion evidence differs: cycle {index}')
