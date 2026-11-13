#!/usr/bin/env python3
"""Check the conformance runner's refusal predicates."""
import os
import secrets
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch
from phase1 import (Failure, capability_environment, control_not_completed, equal_spines, hook_blocked,
                    kill_marked_processes, mcp_tool_calls, require_capability_isolation, require_driver,
                    require_driver_trace, require_mcp_calls, supported_has_events)

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
        if not Path('/proc/self/environ').is_file(): self.skipTest('Linux /proc required')
        marker=secrets.token_hex(16); env=os.environ.copy(); env['MAESTRO_PHASE1_RUN']=marker
        p=subprocess.Popen(['sleep','30'],env=env,start_new_session=True)
        try:
            kill_marked_processes(marker)
            try: result=p.wait(timeout=2)
            except subprocess.TimeoutExpired: self.fail('detached process survived cleanup')
            self.assertNotEqual(result,0)
        finally:
            if p.poll() is None: p.kill(); p.wait()

    def test_proc_absent(self):
        with patch('phase1.Path') as path:
            path.return_value.is_dir.return_value=False
            kill_marked_processes('test')
            path.return_value.iterdir.assert_not_called()

    def test_marked_process_is_killed(self):
        proc=MagicMock(); proc.name='42'; (proc/'environ').read_bytes.return_value=b'MAESTRO_PHASE1_RUN=marker\0'
        with patch('phase1.Path') as path, patch('phase1.os.getpid',return_value=1), patch('phase1.os.kill') as kill:
            path.return_value.is_dir.return_value=True; path.return_value.iterdir.return_value=[proc]
            kill_marked_processes('marker')
            kill.assert_called_once()

    def test_capability_environment(self):
        with tempfile.TemporaryDirectory() as tmp:
            Path(tmp,'maestro').touch()
            env={'PATH':tmp+os.pathsep+'/usr/bin','MAESTRO_SESSION':'s','MAESTRO_DIRECT_TOKEN':'t',
                 'MAESTRO_DIRECT_REGISTRY':'r','MAESTRO_CALLER_CWD':'c','MAESTRO_BRIDGE_CREDENTIAL':'b'}
            clean=capability_environment(env)
            self.assertEqual(clean,{'PATH':'/usr/bin'})
            self.assertEqual(env['MAESTRO_DIRECT_TOKEN'],'t')
            require_capability_isolation(clean)
            with self.assertRaises(Failure): require_capability_isolation(env)

    def test_mcp_calls(self):
        require_mcp_calls(['maestro_flow_claim','maestro_flow_complete'])
        with self.assertRaises(Failure): require_mcp_calls(['maestro_flow_claim'])
        calls,_=mcp_tool_calls('\n'.join(['{"type":"message","text":"maestro_flow_claim maestro_flow_complete"}',
                                         '{"type":"tool_use","tool_name":"mcp_maestro_maestro_flow_claim","tool_id":"1","parameters":{"instance":"fi_test"}}',
                                         '{"type":"tool_result","tool_id":"1","status":"success","output":"{\\"ok\\":true,\\"seq\\":2}"}']),'gemini','fi_test')
        self.assertEqual(calls,['maestro_flow_claim'])
        with self.assertRaises(Failure): require_mcp_calls(calls)
        agy='{"event":"step_update","step_update":{"state":"DONE","step_type":"tool","tool_name":"call_mcp_tool","tool_info":{"parameters":{"ServerName":"maestro","ToolName":"maestro_flow_claim","Arguments":{"instance":"fi_test"}},"output":"{\\"ok\\":true,\\"seq\\":2}"}}}'
        calls,_=mcp_tool_calls(agy,'agy','fi_test'); self.assertEqual(calls,['maestro_flow_claim'])
        self.assertEqual(mcp_tool_calls(agy.replace('"ServerName":"maestro"','"ServerName":"other"'),'agy','fi_test')[0],[])
        self.assertEqual(mcp_tool_calls(agy,'agy','fi_other')[0],[])
        complete=agy.replace('maestro_flow_claim','maestro_flow_complete').replace('"instance":"fi_test"','"claim_token":"fi_test.secret"')
        calls,excerpt=mcp_tool_calls(agy+'\n'+complete,'agy','fi_test')
        require_mcp_calls(calls); self.assertEqual(excerpt[1]['arguments']['claim_token'],'[redacted]')

    def test_driver_availability(self):
        require_driver('codex',True); require_driver('gemini',False)
        with self.assertRaises(Failure): require_driver('codex',False)
        with self.assertRaises(Failure): require_driver('claude',False)

    def test_driver_traces(self):
        claude=[{'tool':'Skill','skill':'flow-verify-v1'},{'tool':'Agent','subagent_type':'verifier'}]
        codex=[{'tool':'skill','skill':'flow-verify-v1'},{'tool':'spawn_agent','task_name':'verifier','model':'gpt-6.1-terra'}]
        require_driver_trace('claude',claude,'gpt-6.1-terra')
        require_driver_trace('codex',codex,'gpt-6.1-terra')
        for harness,calls in [('claude',claude[:1]),('codex',codex[:1]),('codex',codex)]:
            with self.assertRaises(Failure): require_driver_trace(harness,calls,'wrong-model')

if __name__ == '__main__': unittest.main()
