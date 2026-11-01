//! rightkit-qa: the launch chain a pooled QA scenario runs before it starts an app, on a real QA workspace: two scenario worlds are
//! built (directories, binding, control token), the launch guard admits each world and every path it hands the app (home, data,
//! temp, control socket, token file), the launch's token step reads the world's token file, the app's control socket fits the Unix
//! limit, and a world's guard refuses the other world's socket. Then the workspace is disposed.
use rightkit_qa::config::HostConfig;
use rightkit_qa::control::token;
use rightkit_qa::pool::{world_workspace, HomeTemplate, WorldPlan};
use rightkit_qa::userdata::{Guard, Rule};
use rightkit_qa::workspace;
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let cache = ctx.dir("qa")?;
    let ws = workspace::create(&cache, None, "rke2e").map_err(|e| format!("workspace: {e}"))?;
    let result = (|| -> Result<Value, String> {
        let template = HomeTemplate::capture(&ws.home).map_err(|e| format!("template: {e}"))?;
        let plan = WorldPlan::new(ws.run_id.clone(), ws.home.clone(), &HostConfig::default(), 2).map_err(|e| format!("plan: {e}"))?;
        let (wa, wb) = (plan.world(0).map_err(|e| e.to_string())?, plan.world(1).map_err(|e| e.to_string())?);
        let mut seen = vec![];
        for (world, other) in [(&wa, &wb), (&wb, &wa)] {
            let w = world_workspace(&ws, &template, world, &[]).map_err(|e| format!("world: {e}"))?;
            let guard = Guard::for_workspace(&w).map_err(|e| format!("guard refused its own world {}: {e:?}", world.home.display()))?;
            let env: Vec<(String, String)> = w.env.clone().into_iter().collect();
            guard.check_env(&env).map_err(|e| format!("guard refused the world's env: {e:?}"))?;
            for p in [&world.home, &world.data_dir, &world.temp, &world.control_socket, &world.control_token_file] {
                guard.check_dir(p).map_err(|e| format!("guard refused {}: {e:?}", p.display()))?;
            }
            let named = token::named_for_launch(&w.env, &[]);
            let (auth, file) = token::for_launch(named.as_deref(), &ws.root, "probe").map_err(|e| format!("token step: {e}"))?;
            if file != world.control_token_file || auth.len() != 64 {
                return Err(format!("the launch read {} (token length {}), not the world's token file", file.display(), auth.len()));
            }
            let socket_len = world.temp.as_os_str().len() + 1 + 33;
            if cfg!(unix) && socket_len > 103 {
                return Err(format!("the app's fallback control socket under {} would be {socket_len} bytes, over 103", world.temp.display()));
            }
            let crossed = guard.check_dir(&other.control_socket).err().map(|e| e.rule == Rule::OutsideQaRoot).unwrap_or(false);
            if !crossed {
                return Err("a world's guard admitted the other world's control socket".into());
            }
            seen.push(json!({ "world": world.dir, "socket_bytes": socket_len }));
        }
        Ok(json!({ "worlds": seen }))
    })();
    let report = workspace::dispose(&ws, false, 3, 200);
    let mut v = result?;
    v["disposed"] = json!(format!("{report:?}").len() > 0);
    Ok(v)
}
