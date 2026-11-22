#!/usr/bin/env python3
"""How and when the bot dies against the simulated squad: kills at death, time and cause
of death, attackers and HP history at death, cubes and weapons, kill timing.

    target/release/sim --inproc "" --fill squad --seed 7000 --matches 400 --jobs 3 \
        --stop-when-focus-dies --detail > /tmp/ev.out 2>/dev/null
    python3 scripts/death_table.py /tmp/ev.out

Reads the per-match lines of `sim --detail` (the bot's own counters, `Diag` in
crates/bot/src/brain.rs, include the state at its last decision). Simulator numbers:
the squad is a guess, see crates/sim/README.md.
"""
import sys,re,statistics as st
from collections import Counter,defaultdict
M=[]
cur=None
for l in open(sys.argv[1]):
    if l.startswith('seed='):
        m=re.search(r'place=(\d+) kills=(\d+) pts=(\d+) (died t=([\d.]+)s cause=(\w+)(?: by (\d+) \((\w+)-\d+\))?|alive at t=([\d.]+)s)',l)
        cur=dict(place=int(m.group(1)),kills=int(m.group(2)),pts=int(m.group(3)),t=float(m.group(5) or m.group(9)),cause=m.group(6) or 'survived',by=m.group(8) or '-',d={},hurt=[],hunters=0)
        cur['cubes']=int(re.search(r'cubes=(\d+)',l).group(1)); cur['dealt']=float(re.search(r'dealt=(\d+)',l).group(1)); cur['taken']=float(re.search(r'taken=(\d+)',l).group(1))
        M.append(cur)
    elif l.startswith('    bot:') and cur:
        cur['d']={k:float(v) for k,v in re.findall(r'(\w+) (-?[\d.]+)',l[8:])}
    elif l.startswith('    hurt by:') and cur:
        cur['hurt']=[(a,int(b),bool(h)) for a,h,b in re.findall(r'(\w+)-\d+(\(H\))? (\d+)',l.split('|')[0])]
        m=re.search(r'hunters at the end: (\d+)',l); cur['hunters']=int(m.group(1)) if m else 0
        m=re.search(r'own gun (\d+)/(\d+)',l); cur['gun']=(int(m.group(1)),int(m.group(2))) if m else (0,0)
n=len(M)
def mean(x): x=list(x); return sum(x)/len(x) if x else float('nan')
def hist(vals,edges):
    c=Counter()
    for v in vals:
        for i,e in enumerate(edges):
            if v<e: c[i]+=1; break
        else: c[len(edges)]+=1
    return ' '.join(f"{('<'+str(edges[i])) if i<len(edges) else '>='+str(edges[-1])}:{100*c[i]/len(vals):.0f}%" for i in range(len(edges)+1))
g=lambda m,k: m['d'].get(k,0.0)
print(f"matches {n}  mean pts {mean(m['pts'] for m in M):.2f}  mean kills {mean(m['kills'] for m in M):.2f}  mean death t {mean(m['t'] for m in M):.0f}s")
print("kills at death:", ' '.join(f"{k}:{100*c/n:.0f}%" for k,c in sorted(Counter(min(m['kills'],10) for m in M).items())))
print("time of death (s):", hist([m['t'] for m in M],[30,45,60,75,90,120,180,300]))
print("cause:", ' '.join(f"{k}:{100*c/n:.0f}%" for k,c in Counter(m['cause'] for m in M).most_common()), '| killer level:', ' '.join(f"{k}:{100*c/n:.0f}%" for k,c in Counter(m['by'] for m in M).most_common()))
print("attackers engaged at death:", ' '.join(f"{int(k)}:{100*c/n:.0f}%" for k,c in sorted(Counter(min(g(m,'end_engaged'),4) for m in M).items())), "| of them within 60 m:", ' '.join(f"{int(k)}:{100*c/n:.0f}%" for k,c in sorted(Counter(min(g(m,'end_engaged60'),3) for m in M).items())))
print("squad hunters when we die:", hist([m['hunters'] for m in M],[2,3,4,6,8,12]))
print("mechs that damaged us in the match:", hist([len([h for h in m['hurt'] if h[0]!='drone']) for m in M],[2,3,4,5,7]))
print("nearest attacker at death (m):", hist([g(m,'end_att_dist') for m in M if g(m,'end_engaged')>0],[45,60,90,120,160]))
print("known mechs within 150 m at death:", hist([g(m,'end_foes150') for m in M],[1,2,3,4,6]))
print("HP%% 20s / 10s / 5s before death (mean): %.0f / %.0f / %.0f ; share with HP>=90%% at -20s: %.0f%%, at -10s: %.0f%%, at -5s: %.0f%%"%(mean(g(m,'end_hp20') for m in M),mean(g(m,'end_hp10') for m in M),mean(g(m,'end_hp5') for m in M),100*mean(g(m,'end_hp20')>=90 for m in M),100*mean(g(m,'end_hp10')>=90 for m in M),100*mean(g(m,'end_hp5')>=90 for m in M)))
print("HP at -20s:", hist([g(m,'end_hp20') for m in M],[25,50,75,90]), "| at -10s:", hist([g(m,'end_hp10') for m in M],[25,50,75,90]))
print("cubes at death:", hist([m['cubes'] for m in M],[1,3,6,10]), "| weapons at death:", ' '.join(f"{ {0:'gun only',2:'mortar',4:'laser',8:'lightning'}.get(int(k),str(int(k)))}:{100*c/n:.0f}%" for k,c in Counter(g(m,'end_weapons') for m in M).most_common()))
print("mode at death:", ' '.join(f"{ ['final','zone','retreat','fight','loot','hold'][int(k)]}:{100*c/n:.0f}%" for k,c in Counter(g(m,'end_mode') for m in M).most_common()))
# kill timing
kt=defaultdict(list)
for m in M:
    prev=0
    for i in range(1,9):
        t=g(m,f'kill_t{i}')
        if t>0: kt[i].append((t,t-prev)); prev=t
print("kill #: mean time (mean gap since previous) [share of matches reaching it]")
print('  '+'  '.join(f"#{i}: {mean(x[0] for x in kt[i]):.0f}s (+{mean(x[1] for x in kt[i]):.0f}s) [{100*len(kt[i])/n:.0f}%]" for i in range(1,9) if kt[i]))
print("seconds alive after the last kill (mean): %.0f ; time before first kill for matches with 0 kills = death time (mean %.0f s, %d matches)"%(mean(m['t']-max([g(m,f'kill_t{i}') for i in range(1,9)]+[0]) for m in M if m['kills']>0), mean(m['t'] for m in M if m['kills']==0), sum(m['kills']==0 for m in M)))
# by kills bucket
print("by kills at death: n | death t | hunters at end | engaged at death | HP%-10s | cubes | laser held% | dmg taken | dealt | gun acc | taken<60m share")
for lo,hi in [(0,0),(1,2),(3,4),(5,6),(7,9),(10,15)]:
    B=[m for m in M if lo<=m['kills']<=hi]
    if not B: continue
    acc=sum(m.get('gun',(0,0))[0] for m in B)/max(1,sum(m.get('gun',(0,0))[1] for m in B))
    print(f"  {lo}-{hi}: {len(B):3d} | {mean(m['t'] for m in B):4.0f} | {mean(m['hunters'] for m in B):.1f} | {mean(g(m,'end_engaged') for m in B):.1f} | {mean(g(m,'end_hp10') for m in B):3.0f} | {mean(m['cubes'] for m in B):.1f} | {100*mean(g(m,'end_weapons')==4 or g(m,'end_weapons')>=4 and int(g(m,'end_weapons'))&4>0 for m in B):3.0f} | {mean(m['taken'] for m in B):5.0f} | {mean(m['dealt'] for m in B):5.0f} | {100*acc:.0f}% | {100*sum(g(m,'dmg_d60') for m in B)/max(1,sum(g(m,'dmg_taken') for m in B)):.0f}%")
# damage by level
lv=defaultdict(float); 
for m in M:
    for a,b,h in m['hurt']: lv[a]+=b
tot=sum(lv.values())
print("damage taken by attacker level:", ' '.join(f"{k}:{100*v/tot:.0f}%" for k,v in sorted(lv.items(),key=lambda x:-x[1])))
print("dps while fighting: dealt/fight_s = %.0f HP/s, taken/engaged_s = %.0f HP/s ; fight %.0f s, loot %.0f s, hold %.0f s per match; laser held %.0f%% of alive time"%(sum(m['dealt'] for m in M)/sum(g(m,'fight_s') for m in M), sum(g(m,'dmg_taken') for m in M)/max(1,sum(g(m,'engaged1_s')+g(m,'engaged2_s')+g(m,'engaged3_s') for m in M)), mean(g(m,'fight_s') for m in M),mean(g(m,'loot_s') for m in M),mean(g(m,'hold_s') for m in M),100*sum(g(m,'laser_s') for m in M)/sum(g(m,'alive_s') for m in M)))
print("seconds with 1 / 2 / 3+ attackers: %.1f / %.1f / %.1f"%(mean(g(m,'engaged1_s') for m in M),mean(g(m,'engaged2_s') for m in M),mean(g(m,'engaged3_s') for m in M)))
print("damage from squad members that were NOT hunters at the end, by kills bucket; killer level; first-contact <60m count; unarmed_s; laser_known_s")
for lo,hi in [(0,0),(1,2),(3,4),(5,6),(7,9),(10,15)]:
    B=[m for m in M if lo<=m['kills']<=hi]
    if not B: continue
    nh=sum(b for m in B for a,b,h in m['hurt'] if not h); al=sum(b for m in B for a,b,h in m['hurt'])
    print(f"  {lo}-{hi}: non-hunter dmg {100*nh/max(1,al):.0f}% | killer:", ' '.join(f"{k}:{100*c/len(B):.0f}%" for k,c in Counter(m['by'] for m in B).most_common()), f"| contact<60: {mean(g(m,'contact_d60') for m in B):.1f} of {mean(g(m,'contact_d60')+g(m,'contact_d110')+g(m,'contact_d160')+g(m,'contact_far') for m in B):.1f} | no 2nd weapon {mean(g(m,'unarmed_s') for m in B):.0f}s | laser known not held {mean(g(m,'laser_known_s') for m in B):.0f}s | attackers in match {mean(len(m['hurt']) for m in B):.1f} | dmg_offtarget {100*sum(g(m,'dmg_offtarget') for m in B)/max(1,sum(g(m,'dmg_taken') for m in B)):.0f}% | blind {100*sum(g(m,'dmg_blind') for m in B)/max(1,sum(g(m,'dmg_taken') for m in B)):.0f}% | rear {100*sum(g(m,'dmg_rear') for m in B)/max(1,sum(g(m,'dmg_taken') for m in B)):.0f}%")
