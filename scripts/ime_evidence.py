"""Release requirement for repeated native Japanese IME cancellation."""
import re


def require_repeated_ime(proof, count=8, require_stages=False):
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
        if require_stages:
            for field, phase, value, selected in [
                ('conversion_preedit', 'conversion-preedit', 'げんぶん', 'げんぶん'),
                ('converted_preedit', 'converted-preedit', '原文', '原文'),
                ('commit_state', 'committed', '原文', ''),
            ]:
                state = cycle.get(field, {})
                if not (
                    state.get('phase') == phase
                    and state.get('value') == value
                    and state.get('selected_text') == selected
                    and state.get('selection_ranges') == 1
                    and state.get('limit_ms') == 10000
                    and 0 <= state.get('elapsed_ms', -1) <= 10000
                ):
                    raise ValueError(f'Native IME stage evidence differs: cycle {index}, {field}')
            if cycle.get('space_presses') != 1 or cycle.get('enter_presses') != 1:
                raise ValueError(f'Native IME conversion key count differs: cycle {index}')
