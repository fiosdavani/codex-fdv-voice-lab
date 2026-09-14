"""TEST ONLY: abrupt exit at a real journal transaction boundary.
Reads only a synthetic fixture whose metadata file names it explicitly.
Never accepts an arbitrary production source or any Voice operation.
"""
import json
import os
from pathlib import Path
import sys
import final_producer as producer
from final_producer import ROOT, local_output, poll_once, write_receipt


if __name__ == '__main__':
    folder = Path(sys.argv[1]).resolve(strict=True)
    marker = json.loads((folder / 'SYNTHETIC-FIXTURE.json').read_text())
    if (not folder.is_relative_to(ROOT) or marker != {'synthetic_only': True}):
        raise SystemExit('SYNTHETIC_FIXTURE_REQUIRED')
    phase = sys.argv[2]
    if phase not in {'before_commit', 'after_commit', 'before_commit_spill'}:
        raise SystemExit('EXPLICIT_TEST_BOUNDARY_REQUIRED')
    if phase == 'before_commit_spill':
        original = producer.open_journal
        def small_cache(path, binding):
            c = original(path, binding)
            c.execute('PRAGMA cache_size=1')
            c.execute('PRAGMA cache_spill=ON')
            return c
        producer.open_journal = small_cache
    def crash_hook(actual):
        if actual == ('before_commit' if phase == 'before_commit_spill' else phase):
            write_receipt(folder / ('REACHED-' + phase + '.json'),
                          {'synthetic_only': True, 'phase': phase,
                           'process_id': os.getpid()})
            os._exit(93 if phase == 'before_commit_spill' else (91 if actual == 'before_commit' else 92))
    poll_once(folder / 'source.sqlite', 'FAKE_THREAD', folder / 'journal.sqlite',
              _test_hook=crash_hook)
    raise SystemExit('CRASH_HOOK_NOT_REACHED')
