#!/usr/bin/env python3
"""Run live phase-1 checks against a disposable Direct-session workspace."""
import argparse
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import signal
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
MAESTRO = Path(os.environ.get('CARGO_TARGET_DIR', str(ROOT / 'target'))).resolve() / 'release/maestro'
DEFINITION = ROOT / 'crates/flow/flows/verify.yaml'

class Failure(Exception):
    pass

def prepare_evidence_dir(path):
    path.mkdir(parents=True,exist_ok=True)
    if any(path.iterdir()):
        raise Failure(f'evidence directory must be empty: {path}')
    return path

def stop_hook_command(maestro, harness, hook):
    return f'{shlex.quote(str(maestro))} flow stop-gate --harness {harness} | tee -a {shlex.quote(str(hook))}'

def capability_settings(server, harness):
    if harness=='gemini':
        return {**server,'tools':{'exclude':['run_shell_command']}}
    return server

def equal_spines(result):
    if not result.get('ok') or result.get('equal') is not True:
        raise Failure('spines differ or could not be compared')

def control_not_completed(events):
    if any(e.get('kind') == 'Completed' and e.get('body', {}).get('node') == 'check' for e in events):
        raise Failure('positive-control check reached Completed')

def supported_has_events(events, harness):
    found = {(e.get('kind'), e.get('body', {}).get('node')) for e in events
             if (e.get('body', {}).get('performer') or {}).get('harness') == harness}
    if not any(('Claimed', node) in found and ('Completed', node) in found
               for _, node in found if node is not None):
        raise Failure(f'{harness} marked supported without matching Claimed and Completed')

def hook_blocked(decisions, instance):
    if not any(d.get('decision')=='block' and instance in d.get('reason','') for d in decisions):
        raise Failure('hook did not block the control instance')

def require_driver(harness, available):
    if harness in ('claude', 'codex') and not available:
        raise Failure(f'{harness} is unavailable')

def require_mcp_calls(calls):
    if not {'maestro_flow_claim', 'maestro_flow_complete'}.issubset(set(calls)):
        raise Failure('MCP claim and complete tool calls are required')

def mcp_tool_calls(transcript, harness, instance):
    calls=[]; excerpt=[]; pending={}
    def matches(verb,args):
        return isinstance(args,dict) and (args.get('instance')==instance if verb=='maestro_flow_claim'
                                         else str(args.get('claim_token','')).startswith(instance+'.'))
    def safe_args(args):
        return {k:('[redacted]' if k=='claim_token' else v) for k,v in args.items()
                if k in ('instance','node_id','session','claim_token')}
    for line in transcript.splitlines():
        try: item=json.loads(line)
        except (ValueError,TypeError): continue
        if harness=='gemini' and item.get('type')=='tool_use' and item.get('tool_name')=='run_shell_command':
            raise Failure('Gemini invoked a shell during the MCP capability check')
        if harness=='agy' and item.get('event')=='step_update' and item.get('step_update',{}).get('tool_name')=='run_command':
            raise Failure('Antigravity invoked a shell during the MCP capability check')
        if harness=='gemini' and item.get('type')=='tool_use':
            name=item.get('tool_name','')
            for verb in ('maestro_flow_claim','maestro_flow_complete'):
                if name=='mcp_maestro_'+verb:
                    pending[item.get('tool_id')]=(verb,item.get('parameters',{}))
        elif harness=='gemini' and item.get('type')=='tool_result' and item.get('tool_id') in pending:
            verb,args=pending.pop(item['tool_id'])
            try: result=json.loads(item.get('output',''))
            except (ValueError,TypeError): continue
            if item.get('status')=='success' and result.get('ok') is True and matches(verb,args):
                calls.append(verb); excerpt.append({'type':'tool_use','tool_name':'mcp_maestro_'+verb,'parameters':safe_args(args),'result':{'ok':True,'seq':result.get('seq')}})
        elif harness=='agy' and item.get('event')=='step_update':
            step=item.get('step_update',{}); info=step.get('tool_info',{}); params=info.get('parameters',{})
            verb=params.get('ToolName'); args=params.get('Arguments',{})
            if step.get('step_type')!='tool' or step.get('state')!='DONE' or step.get('tool_name')!='call_mcp_tool' or params.get('ServerName')!='maestro' or verb not in ('maestro_flow_claim','maestro_flow_complete'):
                continue
            try: result=json.loads(info.get('output',''))
            except (ValueError,TypeError): continue
            if result.get('ok') is True and matches(verb,args):
                calls.append(verb); excerpt.append({'event':'step_update','step_type':'tool','state':'DONE',
                    'tool_name':'call_mcp_tool','server':'maestro','mcp_tool':verb,'arguments':safe_args(args),'result':{'ok':True,'seq':result.get('seq')}})
    return calls,excerpt

def require_driver_trace(harness, calls, model):
    if harness == 'claude':
        valid = any(c.get('tool') == 'Skill' and c.get('skill') == 'flow-verify-v1' for c in calls) and any(
            c.get('tool') == 'Agent' and c.get('subagent_type') == 'verifier' for c in calls)
    else:
        valid = any(c.get('tool') == 'skill' and c.get('skill') == 'flow-verify-v1' for c in calls) and any(
            c.get('tool') == 'spawn_agent' and c.get('task_name') == 'verifier' and c.get('model') == model for c in calls)
    if not valid:
        raise Failure(f'{harness} driver skill and verifier evidence is missing')

def require_mcp_event_match(excerpt, events, harness):
    by_seq={e.get('seq'):e for e in events}
    matches={}
    for item in excerpt:
        verb=item.get('mcp_tool') or item.get('tool_name','').removeprefix('mcp_maestro_')
        e=by_seq.get(item.get('result',{}).get('seq'),{}); body=e.get('body',{})
        if (body.get('performer') or {}).get('harness')==harness and e.get('kind') in ('Claimed','Completed'):
            matches[verb]=(e['kind'],body.get('node'),body.get('iteration'))
    claim=matches.get('maestro_flow_claim'); complete=matches.get('maestro_flow_complete')
    if not claim or not complete or claim[0]!='Claimed' or complete[0]!='Completed' or claim[1:]!=complete[1:]:
        raise Failure('MCP call sequences do not match the same claimed and completed node')

def capability_environment(env, shim):
    env=env.copy()
    for key in list(env):
        if key.startswith('MAESTRO_DIRECT_') or key in ('MAESTRO_SESSION','MAESTRO_CALLER_CWD','MAESTRO_BRIDGE_CREDENTIAL'):
            env.pop(key,None)
    shim.mkdir(parents=True,exist_ok=True)
    for directory in env.get('PATH','').split(os.pathsep):
        try: entries=sorted(Path(directory or '.').iterdir())
        except (FileNotFoundError,NotADirectoryError,PermissionError): continue
        for entry in entries:
            if entry.name=='maestro' or not entry.is_file() or not os.access(entry,os.X_OK): continue
            try: (shim/entry.name).symlink_to(entry.resolve())
            except FileExistsError: pass  # The first executable on PATH wins.
    env['PATH']=str(shim)
    return env

def codex_skill_read(item, repo, skill):
    if item.get('type')!='command_execution' or item.get('status')!='completed' or item.get('exit_code')!=0:
        return False
    command=item.get('command','')
    try:
        outer=shlex.split(command)
        if len(outer)==3 and Path(outer[0]).name in ('bash','sh') and outer[1] in ('-c','-lc'):
            command=outer[2]
        if any(c in command for c in ';|&<>$`\n\r'): return False
        words=shlex.split(command)
    except ValueError: return False
    if not words or Path(words[0]).name not in ('cat','sed','head','rg'): return False
    if not any((repo/word).resolve()==skill.resolve() for word in words[1:] if not word.startswith('-')):
        return False
    output=item.get('aggregated_output','')
    return isinstance(output,str) and any(len(line.strip())>=24 and line in output for line in skill.read_text().splitlines())

def codex_skill_evidence(item, repo, skill):
    if not codex_skill_read(item,repo,skill): return None
    line=next(line for line in skill.read_text().splitlines() if len(line.strip())>=24 and line in item['aggregated_output'])
    return {'tool':'skill','skill':'flow-verify-v1','command':item['command'],
            'status':item['status'],'exit_code':item['exit_code'],'output_line':line}

def require_capability_isolation(env):
    if any(k.startswith('MAESTRO_DIRECT_') or k in ('MAESTRO_SESSION','MAESTRO_CALLER_CWD','MAESTRO_BRIDGE_CREDENTIAL') for k in env) or any(
            (Path(p)/'maestro').exists() for p in env.get('PATH','').split(os.pathsep) if p):
        raise Failure('capability harness has Direct credentials or maestro on PATH')

def kill_marked_processes(marker):
    """Kill even a detached child that inherited this invocation's unique marker (Linux)."""
    needle=('MAESTRO_PHASE1_RUN='+marker).encode()
    # Tests also run on non-Linux hosts; the live runner itself requires /proc.
    if not Path('/proc').is_dir(): return
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name)==os.getpid(): continue
        try:
            if needle in (proc/'environ').read_bytes().split(b'\0'):
                os.kill(int(proc.name),signal.SIGKILL)
        except (FileNotFoundError,ProcessLookupError,PermissionError):
            continue

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--harness', default='claude,codex,gemini,agy')
    parser.add_argument('--timeout', type=int, default=900)
    parser.add_argument('--evidence-dir', required=True, type=Path)
    parser.add_argument('--prove', choices=['spines', 'control', 'support', 'mcp', 'driver-claude', 'driver-codex', 'driver-model', 'unavailable', 'isolation'])
    args = parser.parse_args()
    names = args.harness.split(',')
    if args.timeout < 1 or len(set(names)) != len(names) or any(n not in ('claude','codex','gemini','agy') for n in names):
        parser.error('invalid harness selection or timeout')
    if not MAESTRO.is_file():
        raise Failure('build target/release/maestro first: cargo build --release --locked')
    if not Path('/proc/self/environ').is_file():
        raise Failure('Linux /proc is required to clean up detached harness processes')
    out = prepare_evidence_dir(args.evidence_dir.resolve())
    host_home = Path.home()
    with tempfile.TemporaryDirectory(prefix='maestro-phase1-') as tmp:
        root = Path(tmp); repo = root / 'subject'; repo.mkdir()
        (root / 'workspace/.broomva').mkdir(parents=True)
        (root / 'home/.codex').mkdir(parents=True); (root / 'home/.claude').mkdir(parents=True)
        (root / 'tmp').mkdir(); (repo / 'agents').mkdir()
        for rel in ('.codex/auth.json', '.claude/.credentials.json', '.gemini/antigravity-cli/antigravity-oauth-token'):
            src = host_home / rel; dst = root / 'home' / rel
            if src.is_file():
                dst.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(src, dst)
        def git(*argv):
            subprocess.run(['git', '-C', str(repo), *argv], check=True, stdout=subprocess.DEVNULL)
        git('init','-q'); git('config','core.hooksPath',str(root/'no-hooks'))
        (repo/'hello.sh').write_text('#!/bin/sh\nprintf "hello\\n"\n'); (repo/'hello.sh').chmod(0o755)
        git('add','hello.sh'); git('-c','user.name=Conformance','-c','user.email=conformance@localhost','-c','commit.gpgsign=false','commit','-qm','Seed subject')
        head = subprocess.check_output(['git','-C',str(repo),'rev-parse','HEAD'],text=True).strip()
        shutil.copy2(ROOT/'agents/verifier.agent.yaml', repo/'agents/verifier.agent.yaml')
        subprocess.run([MAESTRO,'agents','render','--root',repo],check=True,stdout=subprocess.DEVNULL)
        for h in ('claude','codex'):
            skill = repo / f'.{h}/skills/flow-verify-v1/SKILL.md'; skill.parent.mkdir(parents=True,exist_ok=True)
            subprocess.run([MAESTRO,'flow','driver-skill','--definition',DEFINITION,'--out',skill],check=True)
        dogfood = Path(os.environ.get('MAESTRO_DOGFOOD_SKILL',str(host_home/'.codex/skills/dogfood')))
        if not args.prove and any(h in names and shutil.which(h) for h in ('claude','codex')) and not (dogfood/'SKILL.md').is_file():
            raise Failure('dogfood skill unavailable; set MAESTRO_DOGFOOD_SKILL to its directory')
        if dogfood.is_dir():
            for h in ('claude','codex'): shutil.copytree(dogfood,repo/f'.{h}/skills/dogfood')
        server = {'mcpServers': {'maestro': {'command':str(MAESTRO),'args':['mcp']}}}
        (repo/'.mcp.json').write_text(json.dumps(server))
        (repo/'.gemini/settings.json').write_text(json.dumps(server))
        (root/'home/.codex/config.toml').write_text(f'[mcp_servers.maestro]\ncommand = "{MAESTRO}"\nargs = ["mcp"]\n')
        agy_config = root/'home/.gemini/config/mcp_config.json'; agy_config.parent.mkdir(parents=True,exist_ok=True); agy_config.write_text(json.dumps(server))
        registry = root/'direct.json'
        rows = [{'id':h,'cwd':str(repo),'token':'sandbox-token','liveness':'live','harness':{'claude':'claude-code'}.get(h,h)} for h in ('claude','codex','gemini','agy','control')]
        registry.write_text(json.dumps(rows))
        base = os.environ.copy(); base.pop('CLAUDECODE',None)
        base.update({'HOME':str(root/'home'),'TMPDIR':str(root/'tmp'),'CODEX_HOME':str(root/'home/.codex'),
                     'BROOMVA_WORKSPACE':str(root/'workspace'),'BROOMVA_HOME':str(root/'broomva-home'),
                     'MAESTRO_TREE':'worktree','MAESTRO_DIRECT_REGISTRY':str(registry),
                     'MAESTRO_DIRECT_TOKEN':'sandbox-token','MAESTRO_CALLER_CWD':str(repo),
                     'MAESTRO_WORK_ROOT':str(root/'work'), 'MAESTRO_FLEET_STATE':str(root/'fleet'),
                     'MAESTRO_PASEO_ASK_LOCKS':str(root/'ask-locks'), 'MAESTRO_ASK_LEDGER':str(root/'ask_ledger.py'),
                     'PATH':str(MAESTRO.parent)+os.pathsep+base.get('PATH','')})
        def environment(h):
            e = base.copy(); e['MAESTRO_SESSION'] = f'direct:-/{h}'
            if h == 'gemini':
                e['HOME'] = str(host_home); e.pop('GEMINI_API_KEY',None); e.pop('GOOGLE_API_KEY',None)
                e.setdefault('GEMINI_CLI_HOME',str(host_home/'.local/share/broomva/gemini-vertex/home'))
                e.setdefault('GOOGLE_APPLICATION_CREDENTIALS',str(host_home/'.config/gcloud/application_default_credentials.json'))
                e.setdefault('GOOGLE_GENAI_USE_VERTEXAI','true')
                e.setdefault('GOOGLE_CLOUD_LOCATION','global')
                e['GEMINI_CLI_TRUST_WORKSPACE']='true'
            return e
        def run(cmd, h, timeout=None, capability=False):
            cmd=[str(c) for c in cmd]
            marker=secrets.token_hex(16); env=environment(h); env['MAESTRO_PHASE1_RUN']=marker
            if capability:
                env=capability_environment(env,root/'capability-bin')
                require_capability_isolation(env)
                (out/f'{h}-isolation.json').write_text(json.dumps({
                    'maestro_on_path':shutil.which('maestro',path=env['PATH']) is not None,
                    'direct_or_session_env_keys':sorted(k for k in env if k.startswith('MAESTRO_DIRECT_')
                        or k in ('MAESTRO_SESSION','MAESTRO_CALLER_CWD','MAESTRO_BRIDGE_CREDENTIAL')),
                    'mcp_server_env_keys':sorted(server['mcpServers']['maestro'].get('env',{}))
                },indent=2)+'\n')
            process=subprocess.Popen(cmd,cwd=repo,env=env,stdin=subprocess.DEVNULL,
                                     stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
            try:
                stdout,stderr=process.communicate(timeout=timeout or args.timeout)
                return subprocess.CompletedProcess(cmd,process.returncode,stdout,stderr)
            except subprocess.TimeoutExpired:
                try: os.killpg(process.pid,signal.SIGKILL)
                except ProcessLookupError: pass
                kill_marked_processes(marker)
                stdout,stderr=process.communicate()
                return subprocess.CompletedProcess(cmd,124,stdout,stderr)
            finally:
                kill_marked_processes(marker)
        def cli(h,*cmd,ok=True):
            p=run([MAESTRO,*cmd],h)
            try: result=json.loads(p.stdout)
            except (ValueError,TypeError): raise Failure(f'maestro {cmd[0]} did not return JSON: {p.stderr}')
            if ok and p.returncode: raise Failure(f'maestro {cmd[0]} failed: {result}')
            return result,p.returncode
        def start(h,check='./hello.sh | grep -x hello'):
            params=json.dumps({'repo':str(repo),'head':head,'check':check,'surface':'./hello.sh'})
            return cli(h,'flow','start','--definition',DEFINITION,'--params',params)[0]['instance']
        def events(instance):
            return [json.loads(s) for s in (root/'workspace/.broomva/flows/instances'/instance/'events.jsonl').read_text().splitlines()]
        def step(h,i,node):
            token=cli(h,'flow','claim','--instance',i,'--node-id',node)[0]['claim_token']
            return cli(h,'flow','complete','--claim-token',token,ok=False)
        def record_events(name,i):
            rows=[]
            for e in events(i):
                b=e['body']; rows.append({'seq':e['seq'],'kind':e['kind'],'node':b.get('node'),
                    'performer':b.get('performer'),'verify_exit':(b.get('verify') or {}).get('exit'),'code':b.get('code')})
            (out/f'{name}-events.json').write_text(json.dumps({'instance':i,'events':rows},indent=2)+'\n')
        if args.prove:
            if args.prove=='mcp': require_mcp_calls(['maestro_flow_claim']); return
            if args.prove=='driver-claude': require_driver_trace('claude',[{'tool':'Skill','skill':'flow-verify-v1'}],''); return
            if args.prove=='driver-codex': require_driver_trace('codex',[{'tool':'skill','skill':'flow-verify-v1'}],'gpt-6.1-terra'); return
            if args.prove=='driver-model': require_driver_trace('codex',[{'tool':'skill','skill':'flow-verify-v1'},{'tool':'spawn_agent','task_name':'verifier','model':'wrong'}],'gpt-6.1-terra'); return
            if args.prove=='unavailable': require_driver('codex',False); return
            if args.prove=='isolation': require_capability_isolation({'MAESTRO_DIRECT_TOKEN':'token','PATH':''}); return
            i=start('control', './hello.sh | grep -x hello' if args.prove=='control' else 'false')
            if args.prove=='spines':
                j=start('control'); step('control',i,'pin')
                result,_=cli('control','flow','spine','--instance',i,'--against',j,ok=False)
                (out/'proof-spines.json').write_text(json.dumps(result,indent=2)+'\n'); equal_spines(result)
            if args.prove=='control':
                step('control',i,'pin'); step('control',i,'check'); record_events('proof-control',i); control_not_completed(events(i))
            if args.prove=='support':
                record_events('proof-support',i); supported_has_events(events(i),'gemini')
            return
        def scrub(text):
            if isinstance(text, bytes): text=text.decode(errors='replace')
            text=text.replace('sandbox-token','[redacted]')
            text=re.sub(r'(fi_[0-9a-f]{32})\.[0-9a-f]{32}',r'\1.[redacted]',text)
            return re.sub(r'\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b','[redacted-session]',text,flags=re.I)
        def invoke(h,prompt):
            if h=='claude':
                return run(['claude','-p',prompt,'--output-format','stream-json','--verbose',
                            '--permission-mode','bypassPermissions','--strict-mcp-config','--mcp-config',repo/'.mcp.json'],h)
            if h=='codex':
                return run(['codex','exec','--sandbox','danger-full-access','--json',prompt],h)
            if h in ('gemini','agy'):
                server['mcpServers']['maestro']['env']={k:environment(h)[k] for k in
                    ('MAESTRO_SESSION','MAESTRO_DIRECT_REGISTRY','MAESTRO_DIRECT_TOKEN','MAESTRO_CALLER_CWD',
                     'BROOMVA_WORKSPACE','BROOMVA_HOME','MAESTRO_TREE','MAESTRO_WORK_ROOT')}
            if h=='gemini':
                (repo/'.gemini/settings.json').write_text(json.dumps(capability_settings(server,h)))
                return run([shutil.which('gemini'),'-m',os.environ.get('MAESTRO_GEMINI_MODEL','gemini-3.8-flash'),'-p',prompt,'--output-format','stream-json','--skip-trust','--yolo',
                            '--allowed-mcp-server-names','maestro'],h,capability=True)
            agy_config.write_text(json.dumps(server))
            return run([shutil.which('agy'),'-p',prompt,'--output-format','stream-json','--dangerously-skip-permissions'],h,capability=True)
        results={}; instances={}
        for h in names:
            binary={'claude':'claude','codex':'codex','gemini':'gemini','agy':'agy'}[h]
            if not shutil.which(binary):
                results[h]={'preflight':'unavailable','detail':f'{binary} is not installed'}
                print(f'{h}: unavailable ({binary} is not installed)',flush=True); require_driver(h,False); continue
            if h=='gemini' and not os.environ.get('GOOGLE_CLOUD_PROJECT'):
                results[h]={'preflight':'unavailable','detail':'GOOGLE_CLOUD_PROJECT is required for Gemini Vertex'}
                print('gemini: unavailable (GOOGLE_CLOUD_PROJECT is required)',flush=True); continue
            version=run([shutil.which(binary),'--version'],h,20,capability=h in ('gemini','agy'))
            probe=invoke(h,'Reply with exactly OK. Do not call tools.')
            detail='' if probe.returncode==0 else scrub(probe.stderr or probe.stdout)
            results[h]={'path':shutil.which(binary),'version':scrub(version.stdout).strip(),'preflight':'available' if probe.returncode==0 else 'unavailable',
                        'preflight_exit':probe.returncode,'detail':detail[-2000:]}
            print(f"{h}: {results[h]['preflight']} (preflight exit {probe.returncode})",flush=True)
            if probe.returncode and h in ('claude','codex'):
                require_driver(h,False)
            i=start(h); instances[h]=i
            if h in ('claude','codex'):
                prompt=(f'Use the project skill flow-verify-v1 to drive instance {i} to completion. '
                        'Use Maestro MCP or CLI, read flow next launch_spec and use its verifier brief or agent. '
                        'The surface is ./hello.sh. Keep temporary files under TMPDIR.')
                if h=='claude': prompt+=' Invoke the Skill tool for flow-verify-v1 and the Agent tool for verifier.'
                else: prompt+=' Read .codex/skills/flow-verify-v1/SKILL.md and launch the verifier via the specified spawn_agent brief.'
            else:
                prompt=(f'Use only Maestro MCP tools, never shell or CLI, to claim and complete node pin '
                        f'on instance {i}. Report the exact error if unable.')
            session_dir=root/'home/.codex/sessions'
            before=set(session_dir.rglob('*.jsonl')) if h=='codex' and session_dir.exists() else set()
            p=invoke(h,prompt)
            record_events(h,i)
            if h in ('claude','codex'):
                driver_calls=[]
                if h=='claude':
                    for line in p.stdout.splitlines():
                        try: item=json.loads(line)
                        except ValueError: continue
                        content=item.get('message',{}).get('content',[])
                        for c in content if isinstance(content,list) else []:
                            if isinstance(c,dict) and c.get('type')=='tool_use' and c.get('name') in ('Skill','Agent'):
                                inp=c.get('input',{}); driver_calls.append({'tool':c['name'],'skill':inp.get('skill'),'subagent_type':inp.get('subagent_type')})
                else:
                    for line in p.stdout.splitlines():
                        try: item=json.loads(line).get('item',{})
                        except ValueError: continue
                        read=codex_skill_evidence(item,repo,repo/'.codex/skills/flow-verify-v1/SKILL.md')
                        if read: driver_calls.append(read)
                    for path in set(session_dir.rglob('*.jsonl'))-before if session_dir.exists() else []:
                        for line in path.read_text().splitlines():
                            try: payload=json.loads(line).get('payload',{})
                            except ValueError: continue
                            if payload.get('type')=='function_call' and payload.get('name')=='spawn_agent':
                                try: a=json.loads(payload.get('arguments','{}'))
                                except ValueError: continue
                                driver_calls.append({'tool':'spawn_agent','task_name':a.get('task_name'),'model':a.get('model')})
                match=re.search(r'^  codex:\s*(\S+)\s*$',(ROOT/'agents/verifier.agent.yaml').read_text(),re.MULTILINE)
                if not match: raise Failure('verifier Codex model is missing')
                (out/f'{h}-driver.json').write_text(json.dumps(driver_calls,indent=2)+'\n')
                require_driver_trace(h,driver_calls,match.group(1))
                status,_=cli(h,'flow','status','--instance',i)
                done=any(e['kind']=='Completed' and e['body'].get('node')=='exercise' for e in events(i))
                results[h].update({'exit':p.returncode,'exercise_completed':done,'terminal':status['status']['terminal']})
                if p.returncode or not done or not status['status']['terminal']:
                    raise Failure(f'{h} did not complete verify@1')
                # Retain only the Codex flow-related transcript items, after claim-token redaction.
                if h=='codex':
                    excerpt=[]
                    for line in p.stdout.splitlines():
                        try: item=json.loads(line).get('item',{})
                        except ValueError: continue
                        command=item.get('command','')
                        if item.get('type')=='agent_message' or ('maestro flow' in command and item.get('status')=='completed'):
                            excerpt.append({k:scrub(str(item.get(k,''))) for k in ('type','command','aggregated_output','text')})
                    (out/'codex-trace.json').write_text(json.dumps(excerpt,indent=2)+'\n')
            else:
                try:
                    calls,transcript=mcp_tool_calls(p.stdout,h,i)
                    (out/f'{h}-mcp-calls.json').write_text(json.dumps(transcript,indent=2)+'\n')
                    supported_has_events(events(i),h)
                    require_mcp_calls(calls)
                    require_mcp_event_match(transcript,events(i),h)
                    results[h].update({'capability':'supported','exit':p.returncode})
                except Failure as exc:
                    errors=[]
                    for line in p.stdout.splitlines():
                        try: item=json.loads(line)
                        except (ValueError,TypeError): continue
                        if item.get('type')=='tool_result' and ('error' in str(item.get('status','')).lower()
                                or '"ok":false' in str(item.get('output','')).replace(' ','')):
                            errors.append(str(item.get('output','')))
                    results[h].update({'capability':'unsupported','exit':p.returncode,
                                       'error':str(exc),
                                       'detail':scrub(errors[-1] if errors else p.stderr or p.stdout)[-2000:]})
        if 'claude' in instances and 'codex' in instances:
            comparison,_=cli('claude','flow','spine','--instance',instances['claude'],'--against',instances['codex'],ok=False)
            (out/'spines.json').write_text(json.dumps(comparison,indent=2)+'\n'); equal_spines(comparison)
            requests=[{'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-06-18',
                       'capabilities':{},'clientInfo':{'name':'phase1','version':'1'}}},
                      {'jsonrpc':'2.0','method':'notifications/initialized'},
                      {'jsonrpc':'2.0','id':2,'method':'tools/call','params':{'name':'maestro_flow_spine',
                       'arguments':{'instance':instances['claude'],'against':instances['codex']}}}]
            p=subprocess.run([str(MAESTRO),'mcp'],cwd=repo,env=environment('claude'),
                             input=''.join(json.dumps(v)+'\n' for v in requests),capture_output=True,text=True,timeout=args.timeout)
            replies=[json.loads(line) for line in p.stdout.splitlines()]
            response=next((v.get('result',{}).get('structuredContent') for v in replies if v.get('id')==2),None)
            if p.returncode or not isinstance(response,dict): raise Failure('MCP spine comparison failed')
            (out/'mcp-spines.json').write_text(json.dumps(response,indent=2)+'\n'); equal_spines(response)
        elif 'claude' in names and 'codex' in names:
            raise Failure('Claude and Codex are both required for the spine comparison')
        for h in ('claude','codex'):
            if h not in instances: continue
            # The owner remains the same session across the failing check and the hook run.
            i=start(h,'false'); step(h,i,'pin'); result,code=step(h,i,'check')
            if code==0 or result.get('code')!='verify_failed': raise Failure(f'{h} control did not fail verification')
            control_not_completed(events(i))
            hook=out/f'{h}-hook.jsonl'
            hook.write_text('')
            command=stop_hook_command(MAESTRO,h,hook)
            settings=repo/('.claude/settings.json' if h=='claude' else '.codex/hooks.json')
            settings.write_text(json.dumps({'hooks':{'Stop':[{'hooks':[{'type':'command','command':command,'timeout':60}]}]}}))
            install,rc=cli(h,'flow','stop-gate','--check-install','--harness',h,'--settings',settings,ok=False)
            if rc or not install.get('wired'): raise Failure(f'{h} hook not wired')
            if h=='claude': p=invoke(h,'Reply with exactly OK. Do not call tools.')
            else: p=run(['codex','exec','--dangerously-bypass-hook-trust','--sandbox','danger-full-access','--json',
                         'Reply with exactly OK. Do not call tools.'],h)
            control_not_completed(events(i)); record_events(f'{h}-control',i)
            decisions=[json.loads(line) for line in hook.read_text().splitlines()] if hook.exists() else []
            hook_blocked(decisions,i)
            results[h]['stop_hook_decision']='block'
            results[h]['stop_exit']=p.returncode
        (out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
        print(json.dumps(results,indent=2))

if __name__ == '__main__':
    try: main()
    except (Failure, OSError, subprocess.SubprocessError) as e:
        print(f'phase1: {e}',file=sys.stderr); sys.exit(1)
