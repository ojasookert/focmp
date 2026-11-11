//! Explicit console-only helpers for destructive tests in isolated game instances.
//! Keep the native weapon compiler, shot factory, enemy AI and damage callbacks.
use crate::engine::{G,V,real,boolean,truthy,is_struct,key};
use std::{collections::HashSet, sync::atomic::{AtomicBool,Ordering}};
static ACTIVE: AtomicBool = AtomicBool::new(false);
pub fn active() -> bool { ACTIVE.load(Ordering::Relaxed) }
fn object(g:G)->V { g.call("json_parse",&[g.s("{}")]) }
fn array(g:G)->V { g.call("array_create",&[real(0.0)]) }
fn names(g:G,v:V)->Vec<String> {
    let a=g.call("variable_struct_get_names",&[v]);
    (0..g.arr_len(a)).map(|i|g.str_of(&g.arr_get(a,i))).collect()
}
fn call(g:G,target:V,name:&str,args:&[V])->Result<V,String> {
    let method=g.get(target,name);
    if !is_struct(&method)||!truthy(&g.call("is_method",&[method])) {return Err(format!("missing native method {name}"));}
    let a=g.call("array_create",&[real(args.len() as f64)]);
    for (i,v) in args.iter().enumerate(){g.call("array_set",&[a,real(i as f64),*v]);}
    let bound=g.call("method",&[target,method]);
    Ok(g.call("method_call",&[bound,a]))
}
fn tagged_shots(g:G,root:V) {
    fn visit(g:G,v:V,seen:&mut HashSet<(u32,u64)>,depth:usize){
        if depth>64||seen.len()>2048||!seen.insert((v.kind(),v.v)){return;}
        if v.kind()==crate::gm::KIND_ARRAY {
            for i in 0..g.arr_len(v).min(1024){visit(g,g.arr_get(v,i),seen,depth+1);}
        } else if is_struct(&v)&&!truthy(&g.call("is_method",&[v])) {
            let fields=names(g,v);
            g.set(v,"focmp_fuzz_shot",boolean(true));
            for field in fields {if !matches!(field.as_str(),"infotxt"|"draw_info"){visit(g,g.get(v,&field),seen,depth+1);}}
        }
    }
    visit(g,root,&mut HashSet::new(),0);
}
fn destroy_enemy(g:G,e:V){
    let gun=g.get(e,"Gun");if g.exists(gun){g.call("instance_destroy",&[gun,boolean(false)]);}
    if g.exists(e){g.call("instance_destroy",&[e,boolean(false)]);}
}
fn cleanup(g:G){
    // Melee hitboxes use a small native shot struct rather than a gun's tagged
    // definition. Destroy them before their enemy Owner can be removed.
    for hit in g.instances("objMeleeEnemy") {g.call("instance_destroy",&[hit]);}
    // Child effects can carry a network shot identity without retaining the
    // tagged definition. Cleanup events may create more children, so drain them
    // before the next case rather than leaving a previous shot in the arena.
    for _ in 0..8 {
        let mut removed = false;
        // Snapshot before destroying, because instance indices change on removal.
        let n=g.call("instance_number",&[real(-3.0)]).as_f64().unwrap_or(0.0) as usize;
        let all:Vec<V>=(0..n).map(|i|g.call("instance_find",&[real(-3.0),real(i as f64)])).collect();
        for inst in all {
            if !g.exists(inst){continue;}
            let shot=g.get(inst,"shot");
            if g.num(inst,"focmp_shot_id") != 0.0 || (is_struct(&shot)&&truthy(&g.get(shot,"focmp_fuzz_shot"))) {
                g.call("instance_destroy",&[inst,boolean(false)]);
                removed = true;
            }
        }
        if !removed { break; }
    }
    for e in g.instances("objEnemyMASTER") {
        if truthy(&g.get(e,"focmp_fuzz_enemy")){destroy_enemy(g,e);}
    }
}
pub fn tick(g:G){
    if !active(){return;}
    if g.room_name()!=g.str_of(&g.global("focmp_fuzz_room"))||g.first("objPlayer").is_none(){ACTIVE.store(false,Ordering::Relaxed);return;}
    // Tag enemy shot definitions too, so no projectile from a previous case
    // survives cleanup on either host or client.
    for e in g.instances("objEnemyMASTER"){
        let gun=g.get(e,"Gun");
        if g.exists(gun)&&!truthy(&g.get(gun,"focmp_fuzz_tagged")){
            tagged_shots(g,g.get(gun,"shots"));g.set(gun,"focmp_fuzz_tagged",boolean(true));
        }
    }
}
pub fn console(g:G,a:&[&str])->String {
    match command(g,a){Ok(v)=>g.str_of(&g.call("json_stringify",&[v])),Err(e)=>format!("error: {e}")}
}
fn command(g:G,a:&[&str])->Result<V,String>{
    let out=object(g);g.call("variable_global_set",&[g.s("focmp_fuzz_output"),out]);
    let Some(p)=g.first("objPlayer") else{return Err("enter a co-op fight first".into())};
    let cards=g.get(p,"Cards");let inventory=g.get(g.get(p,"oInventory"),"inventory");
    match a {
        ["catalog"]=>{
            g.set(out,"build",g.s(crate::version::BUILD));
            g.set(out,"weapons",g.get(cards,"gun_list_player"));
            let rows=array(g);g.set(out,"cards",rows);
            let b=g.get(cards,"B");
            for category in ["bullits","booms","melees","flexmod"] {
                let group=g.get(b,category);
                for name in names(g,group) {
                    let card=g.get(group,&name);
                    if !is_struct(&card)||g.get(card,"name").as_str().is_none(){continue;}
                    let row=object(g);g.call("array_push",&[rows,row]);
                    g.set(row,"category",g.s(category));g.set(row,"name",g.s(&name));
                    g.set(row,"tier",g.get(card,"tier"));g.set(row,"damage_type",g.get(card,"damage_type"));
                }
            }
            let enemies=array(g);g.set(out,"enemies",enemies);
            // These are concrete combat actors. Bosses and hub-only targets need
            // their native arenas; report those exclusions instead of claiming coverage.
            for name in ["objEnemyBoom","objEnemyDog","objEnemyMeleeBig","objEnemyMeleeRiot","objEnemyShoot","objEnemyShootShielded"] {
                if truthy(&g.call("object_exists",&[g.asset(name)])){g.call("array_push",&[enemies,g.s(name)]);}
            }
            g.set(out,"excluded",g.s("objEnemyMelee is not standalone-safe (native chase calls undefined move_start); bosses, hub-only targets, perks, difficulty and utility cards require dedicated setup. Weapon-card cases cover B.bullits/booms/melees/flexmod including native tier variants."));
        }
        ["begin"]=>{
            if active(){return Err("fuzz session already active".into());}
            if super::rooms::RUN_SEED.load(Ordering::Relaxed)==0||!g.room_name().starts_with("Room_fight"){return Err("requires a connected co-op fight".into());}
            let saved=g.call("variable_clone",&[g.get(inventory,"guns")]);
            g.call("variable_global_set",&[g.s("focmp_fuzz_guns"),saved]);
            g.call("variable_global_set",&[g.s("focmp_fuzz_room"),g.s(&g.room_name())]);
            for name in ["hp","hp_max","gun_i","x","y"]{g.set(p,&format!("focmp_saved_{name}"),g.get(p,name));}
            g.set(p,"hp",real(10000.0));g.set(p,"hp_max",real(10000.0));
            g.set(p,"cursor_pos",g.call("variable_clone",&[g.get(p,"cursor_pos")]));
            ACTIVE.store(true,Ordering::Relaxed);g.set(out,"active",boolean(true));
        }
        ["end"]=>{
            if active(){
                cleanup(g);g.set(inventory,"guns",g.global("focmp_fuzz_guns"));
                for name in ["hp","hp_max","gun_i","x","y"]{g.set(p,name,g.get(p,&format!("focmp_saved_{name}")));}
                call(g,p,"select_gun",&[g.get(p,"gun_i")])?;
            }
            ACTIVE.store(false,Ordering::Relaxed);g.set(out,"active",boolean(false));
        }
        ["state"]=>{
            g.set(out,"active",boolean(active()));g.set(out,"room",g.s(&g.room_name()));
            g.set(out,"hp",g.get(p,"hp"));g.set(out,"dead",g.get(p,"dead"));
            let gun=g.get(p,"Gun");g.set(out,"weapon",g.get(g.get(gun,"Gun"),"name"));
            g.set(out,"x",g.get(p,"x"));g.set(out,"y",g.get(p,"y"));
            let tip=g.get(gun,"gun_tip");g.set(out,"tip_x",g.get(tip,"x"));g.set(out,"tip_y",g.get(tip,"y"));
            let enemies=array(g);g.set(out,"enemies",enemies);
            for e in g.instances("objEnemyMASTER") {
                let row=object(g);g.call("array_push",&[enemies,row]);
                g.set(row,"gid",g.get(e,"focmp_gid"));g.set(row,"object",g.call("object_get_name",&[g.get(e,"object_index")]));
                for name in ["hp","dead","x","y","onfire","poisoned","frozen","visible","draw_enemy"]{g.set(row,name,g.get(e,name));}
                let target=g.get(e,"Player");
                if g.exists(target){for name in ["x","y","dead"]{g.set(row,&format!("target_{name}"),g.get(target,name));}}
                let history=g.get(g.get(e,"fsm"),"__history");
                if g.arr_len(history)>0{g.set(row,"state",g.arr_get(history,0));}
            }
        }
        _ if !active()=>return Err("call coop fuzz begin first (isolated QA instances only)".into()),
        ["cleanup"]=>{cleanup(g);g.set(out,"ok",boolean(true));}
        ["seed",seed]=>{let seed=seed.parse::<u32>().map_err(|_|"invalid seed")?;g.call("random_set_seed",&[real(seed as f64)]);g.set(out,"seed",real(seed as f64));}
        ["aim",x,y]=>{
            let x=x.parse::<f64>().map_err(|_|"invalid x")?;let y=y.parse::<f64>().map_err(|_|"invalid y")?;
            if !x.is_finite()||!y.is_finite(){return Err("invalid aim".into());}
            let cursor=g.get(p,"cursor_pos");g.set(cursor,"x",real(x));g.set(cursor,"y",real(y));g.set(out,"ok",boolean(true));
        }
        ["equip",weapon,category,name]=>{
            let proto=g.get(g.get(g.get(cards,"G"),"guns"),weapon);
            if !is_struct(&proto){return Err("unknown weapon".into());}
            let def=g.call("variable_clone",&[proto]);
            g.set(p,"focmp_fuzz_loadout",def); // GC root during native compilation
            if *name!="default" {
                if !["bullits","booms","melees","flexmod"].contains(category){return Err("unsupported card category".into());}
                let card=g.get(g.get(g.get(cards,"B"),category),name);
                if !is_struct(&card){return Err("unknown card".into());}
                let list=array(g);g.set(def,"cards",list);
                g.call("array_push",&[list,g.call("variable_clone",&[card])]);
                if *category=="flexmod" {let baseline=g.arr_get(g.get(proto,"cards"),0);g.call("array_push",&[list,g.call("variable_clone",&[baseline])]);}
            }
            g.call("array_set",&[g.get(inventory,"guns"),real(0.0),def]);
            call(g,p,"select_gun",&[real(0.0)])?;
            let gun=g.get(p,"Gun");tagged_shots(g,g.get(gun,"shots"));
            g.set(out,"weapon",g.get(g.get(gun,"Gun"),"name"));
            g.set(out,"shots",g.get(gun,"shots_len"));
        }
        ["append",category,name]=>{
            if !["bullits","booms","melees","flexmod"].contains(category){return Err("unsupported card category".into());}
            let card=g.get(g.get(g.get(cards,"B"),category),name);
            if !is_struct(&card){return Err("unknown card".into());}
            let def=g.get(p,"focmp_fuzz_loadout");if !is_struct(&def){return Err("equip first".into());}
            let list=g.get(def,"cards");let card=g.call("variable_clone",&[card]);
            g.call("array_push",&[list,card]);call(g,p,"select_gun",&[real(0.0)])?;
            tagged_shots(g,g.get(g.get(p,"Gun"),"shots"));g.set(out,"ok",boolean(true));
        }
        ["spawn",name,x,y]=>{
            if super::enemies::PUPPET_MODE.load(Ordering::Relaxed){return Err("spawn on host only".into());}
            if !["objEnemyBoom","objEnemyDog","objEnemyMeleeBig","objEnemyMeleeRiot","objEnemyShoot","objEnemyShootShielded"].contains(name){return Err("enemy needs a dedicated arena/setup".into());}
            let x=x.parse::<f64>().map_err(|_|"invalid x")?;let y=y.parse::<f64>().map_err(|_|"invalid y")?;
            if !x.is_finite()||!y.is_finite(){return Err("invalid spawn".into());}
            let e=g.call("instance_create_depth",&[real(x),real(y),real(400.0),g.asset(name)]);
            if !g.exists(e){return Err("native enemy creation failed".into());}
            g.set(e,"focmp_fuzz_enemy",boolean(true));g.set(out,"instance",real(key(&e) as u32 as f64));
        }
        ["enemy-fire",gid,angle]=>{
            if super::enemies::PUPPET_MODE.load(Ordering::Relaxed){return Err("host only".into());}
            let gid=gid.parse::<u16>().map_err(|_|"invalid gid")?;
            let angle=angle.parse::<f64>().map_err(|_|"invalid angle")?;
            if !angle.is_finite(){return Err("invalid angle".into());}
            let enemy=g.instances("objEnemyMASTER").into_iter().find(|e|g.num(*e,"focmp_gid") as u16==gid).ok_or("enemy missing")?;
            let gun=g.get(enemy,"Gun");g.set(gun,"mag_bullets",g.get(gun,"mag_size"));g.set(gun,"mag_reloading",boolean(false));
            g.set(gun,"gun_wait",real(-1000.0));g.set(gun,"gun_wait_updated",boolean(true));g.set(gun,"gun_shoot_time",real(-1000.0));
            g.set(gun,"image_angle",real(angle));g.set(gun,"current_spread",real(0.0));g.set(gun,"gun_tip_setted",boolean(false));
            let before=g.num(gun,"mag_bullets");call(g,gun,"fire",&[])?;g.set(out,"ammo_used",real(before-g.num(gun,"mag_bullets")));
        }
        ["fire"]=>{
            let gun=g.get(p,"Gun");g.set(gun,"mag_bullets",g.get(gun,"mag_size"));g.set(gun,"mag_reloading",boolean(false));
            g.set(gun,"gun_wait",real(-1000.0));g.set(gun,"gun_shoot_time",real(-1000.0));
            let before=g.num(gun,"mag_bullets");call(g,gun,"fire",&[])?;
            g.set(out,"ammo_used",real(before-g.num(gun,"mag_bullets")));
        }
        _=>return Err("usage: coop fuzz catalog|begin|end|state|cleanup|seed N|aim X Y|equip WEAPON CATEGORY CARD|spawn ENEMY X Y|fire".into()),
    }
    Ok(out)
}
