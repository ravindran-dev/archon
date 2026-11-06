"""Unprivileged Nizho CLI client. No privileged executables, no package engine."""
import json
import sys
from gi.repository import Gio, GLib

def safe(value): return ''.join(c for c in str(value) if c.isprintable())

BUS='org.nizho.Archon1'
PATH='/org/nizho/Archon1'
IFACE=BUS+'.Manager'

def main(request):
    bus=Gio.bus_get_sync(Gio.BusType.SYSTEM,None)
    def call(method, signature=None, args=(), interactive=False):
        result=bus.call_sync(BUS,PATH,IFACE,method,GLib.Variant(signature,args) if signature else None,None,Gio.DBusCallFlags.ALLOW_INTERACTIVE_AUTHORIZATION if interactive else Gio.DBusCallFlags.NONE,120000,None)
        return result.unpack()[0] if result.n_children() else None
    command=request['command']
    if command=='capabilities':
        print(json.dumps(call('GetCapabilities'),indent=2));return
    if command in ('installed','repositories','history'):
        result=call({'installed':'GetInstalledPackages','repositories':'GetRepositories','history':'GetTransactionHistory'}[command], '(uu)' if command=='history' else None,(0,100) if command=='history' else ())
        print(json.dumps(json.loads(result) if isinstance(result,str) else result,indent=2));return
    if command=='search':
        offset=0
        while True:
            rows=call('SearchPackages','(ssuu)',(request['query'],'',offset,100 if request.get('all') else 50))
            for row in rows: print(f'{safe(row[0])}  [{safe(row[4])}]  {safe(row[2])}'+('  (installed)' if row[6] else ''))
            if not request.get('all') or len(rows)<100:return
            offset+=100
    packages=request.get('packages',[])
    if command=='update':
        candidates=call('GetAvailableUpdates');packages=[row[0] for row in candidates]
    for package in packages:
        rows=call('GetInstalledPackages') if command in ('remove','update') else call('SearchPackages','(ssuu)',(package.split('/')[1] if '/' in package else package,'',0,100))
        matches=[r for r in rows if r[0]==package or r[0].split('/')[1]==package]
        if len(matches)!=1:raise ValueError('Package must identify one exact application reference; use archon search to inspect available references')
        row=matches[0]
        if command=='info':print(json.dumps(json.loads(call('GetPackageDetails','(ss)',(row[0],row[4]))),indent=2));continue
        plan=call('PlanTransaction','(sss)',(command,row[0],row[4]))
        print(json.dumps(json.loads(plan[3]),indent=2))
        if request.get('dry_run'):
            call('CancelTransaction','(s)',(plan[0],));continue
        if not request.get('yes'):
            if not sys.stdin.isatty() or input('Authorize and apply this transaction? [y/N] ').strip().lower() not in ('y','yes'):
                call('CancelTransaction','(s)',(plan[0],));raise ValueError('Transaction cancelled')
        loop=GLib.MainLoop();outcome=[]
        def event(connection,sender,path,interface,name,parameters):
            args=parameters.unpack()
            if args[0]!=plan[0]:return
            if name=='TransactionProgress':print(f'{args[1]}% {safe(args[2])}',flush=True)
            elif name in ('TransactionCompleted','TransactionFailed'):
                outcome.append(None if name=='TransactionCompleted' else args[1]);loop.quit()
        subscription=bus.signal_subscribe(BUS,IFACE,None,PATH,None,Gio.DBusSignalFlags.NONE,event)
        try:
            call('CommitTransaction','(s)',(plan[0],),True)
            GLib.timeout_add_seconds(3600,lambda:(loop.quit(),False)[1]);loop.run()
            if not outcome:raise ValueError('Timed out waiting for transaction; inspect archon history before retrying')
            if outcome[0]:raise ValueError(outcome[0])
            print('Transaction completed')
        finally:bus.signal_unsubscribe(subscription)

try:main(json.loads(sys.argv[1]))
except Exception as error:
    print(f'archon: {error}',file=sys.stderr);sys.exit(1)
