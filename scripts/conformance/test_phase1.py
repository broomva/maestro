#!/usr/bin/env python3
"""Check the conformance runner's refusal predicates."""
import os
import secrets
import subprocess
import unittest
from phase1 import Failure, control_not_completed, equal_spines, hook_blocked, kill_marked_processes, supported_has_events

class Guards(unittest.TestCase):
    def test_spines(self):
        equal_spines({'ok':True,'equal':True})
        with self.assertRaises(Failure): equal_spines({'ok':False,'equal':False})

    def test_control(self):
        control_not_completed([{'kind':'Refused','body':{'node':'check'}}])
        with self.assertRaises(Failure): control_not_completed([{'kind':'Completed','body':{'node':'check'}}])

    def test_support(self):
        def event(kind, harness='gemini'):
            return {'kind':kind,'body':{'node':'pin','performer':{'harness':harness}}}
        supported_has_events([event('Claimed'),event('Completed')],'gemini')
        with self.assertRaises(Failure): supported_has_events([event('Claimed')],'gemini')
        with self.assertRaises(Failure): supported_has_events([event('Claimed'),event('Completed','agy')],'gemini')

    def test_hook_names_control(self):
        hook_blocked([{'decision':'block','reason':'fi_control remains open'}], 'fi_control')
        with self.assertRaises(Failure): hook_blocked([{'decision':'block','reason':'fi_other remains open'}], 'fi_control')

    def test_detached_process_cleanup(self):
        marker=secrets.token_hex(16); env=os.environ.copy(); env['MAESTRO_PHASE1_RUN']=marker
        p=subprocess.Popen(['sleep','30'],env=env,start_new_session=True)
        try:
            kill_marked_processes(marker)
            try: result=p.wait(timeout=2)
            except subprocess.TimeoutExpired: self.fail('detached process survived cleanup')
            self.assertNotEqual(result,0)
        finally:
            if p.poll() is None: p.kill(); p.wait()

if __name__ == '__main__': unittest.main()
