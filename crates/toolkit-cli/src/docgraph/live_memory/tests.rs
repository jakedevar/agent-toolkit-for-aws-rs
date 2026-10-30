//! T-DG-06b/d: dispatch existing plans and response-dependent expansions unchanged.
use super::*;
use toolkit_core::parse_json;
pub(in crate::docgraph::live_memory) fn json(s: &str) -> Json { parse_json(s).unwrap() }
pub(in crate::docgraph::live_memory) fn policy(gremlin: bool) -> DatabaseEndpoint {
    DatabaseEndpoint::new(if gremlin { "db.us-east-1.neptune.amazonaws.com" } else { "g-abc123def4.us-east-1.neptune-graph.amazonaws.com" }, if gremlin {8182} else {443}, "us-east-1", &[]).unwrap()
}
#[derive(Default)]
struct Spy { calls: Vec<(Route, Json)>, responses: std::collections::VecDeque<Result<Json,String>> }
impl Backend for Spy {
    fn call(&mut self, route: Route, request: &Json, _: Instant) -> Result<Json,String> {
        self.calls.push((route,request.clone()));
        self.responses.pop_front().unwrap_or_else(|| Ok(json(r#"{"results":[]}"#)))
    }
}
fn run(spy: &mut Spy, op: &str, input: &Json, p: Option<&DatabaseEndpoint>) -> Result<Json,String> {
    let plan = memory::plan(op,input)?;
    execute(spy,op,input,&plan,p,Instant::now()+Duration::from_secs(30))
}
#[test]
fn every_memory_backend_dispatches_the_existing_plan_without_modification() {
    for (op,input,route,p) in [
        ("query",json(r#"{"graph_id":"g-abc123def4","query":"RETURN $x","parameters":{"x":[1,true,null,"'雪"]}}"#), Route::Analytics,Some(policy(false))),
        ("create-memory",json("{}"),Route::Create,None),
        ("store-message",json(r#"{"session_id":"s","role":"user","content":"hello","now_ms":1700000000123}"#),Route::Dynamo,None),
        ("remember-entity",json(r#"{"entity_name":"雪'","entity_type":"person","properties":{"a'":1}}"#),Route::Gremlin,Some(policy(true))),
    ] {
        let plan=memory::plan(op,&input).unwrap();
        let mut spy=Spy::default();
        if route==Route::Create {spy.responses.push_back(Ok(json(r#"{"id":"g-abc123def4"}"#)));}
        run(&mut spy,op,&input,p.as_ref()).unwrap();
        assert_eq!(spy.calls,plan.into_iter().map(|r|(route,r)).collect::<Vec<_>>());
    }
}
#[test]
fn ordered_retrieve_expansions_and_hybrid_results_equal_recorded_execution() {
    let input=json(r#"{"graph_id":"g-abc123def4","embedding":[1,0],"entity_name":"a"}"#);
    let mut spy=Spy::default();
    let responses=vec![json(r#"{"results":[{"id":"c'雪","text":"first","score":0.5},{"id":"c2","text":"second","score":1}]}"#),json(r#"{"results":[{"name":"entity"}]}"#),json(r#"{"results":[]}"#),json(r#"{"results":[{"id":"n","text":"next"}]}"#),json(r#"{"results":[]}"#),json(r#"{"results":[]}"#),json(r#"{"results":[]}"#)];
    spy.responses=responses.iter().cloned().map(Ok).collect();
    let result=run(&mut spy,"retrieve",&input,Some(&policy(false))).unwrap();
    assert_eq!(spy.calls.len(),7);
    for (index,(_,request)) in spy.calls.iter().enumerate().skip(1) {
        assert_eq!(request.get("parameters").unwrap().get("cid"),Some(&Json::string(if index<=3 {"c'雪"} else {"c2"})));
        assert!(!request.get("queryString").unwrap().as_str().unwrap().contains("c'雪"));
    }
    let document=Json::object(vec![("calls",Json::Array(spy.calls.iter().zip(&responses).map(|((_,r),v)|Json::object(vec![("request",r.clone()),("response",v.clone())])).collect()))]);
    let mut replay=memory::RecordedMemory::from_document(&document).unwrap();
    assert_eq!(memory::retrieve(&mut replay,"g-abc123def4",&spy.calls[0].1).unwrap(),result);
    assert_eq!(replay.unused(),0);
    let mut spy=Spy::default();
    spy.responses=[Ok(json(r#"{"results":[{"name":"graph"}]}"#)),Ok(json(r#"{"results":[{"name":"vector"}]}"#))].into();
    assert_eq!(run(&mut spy,"hybrid-recall",&input,Some(&policy(false))).unwrap(),json(r#"{"graph_recall":[{"name":"graph"}],"semantic_recall":[{"name":"vector"}]}"#));
    assert!(spy.calls[0].1.get("queryString").unwrap().as_str().unwrap().starts_with("MATCH"));
    assert!(spy.calls[1].1.get("queryString").unwrap().as_str().unwrap().starts_with("CALL"));
}
#[test]
fn consolidation_and_multistep_writes_stop_on_the_first_uncertain_failure() {
    for (op,input) in [
        ("consolidate",json(r#"{"graph_id":"g-abc123def4"}"#)),
        ("remember-embedding",json(r#"{"graph_id":"g-abc123def4","entity_name":"a","description":"b","embedding":[1]}"#)),
        ("ingest-chunk",json(r#"{"graph_id":"g-abc123def4","chunk_id":"c","doc_id":"d","text":"x","sequence":1,"embedding":[1],"prev_chunk_id":"p"}"#)),
    ] {
        let plan=memory::plan(op,&input).unwrap();
        let mut spy=Spy::default();
        spy.responses=[Ok(json(r#"{"results":[]}"#)),Err("unknown outcome".into())].into();
        assert!(run(&mut spy,op,&input,Some(&policy(false))).unwrap_err().contains("unknown outcome"));
        assert_eq!(spy.calls.len(),2);
        assert_eq!(spy.calls[0].1,plan[0]);
        assert_eq!(spy.calls[1].1,plan[1]);
    }
}
#[test]
fn all_requests_are_admitted_before_any_backend_call() {
    let valid=memory::query("g-abc123def4","RETURN 1",json("{}")).unwrap();
    for bad in [json(r#"{"operation":"delete_table","table":"secret"}"#),json(r#"{"graphIdentifier":"g-evil","queryString":"x","language":"OPEN_CYPHER"}"#),json(r#"{"unknown":true}"#),memory::gremlin("g.V()",json("{}"))] {
        let mut spy=Spy::default();
        assert!(execute(&mut spy,"consolidate",&json("{}"),&[valid.clone(),bad],Some(&policy(false)),Instant::now()+Duration::from_secs(10)).is_err());
        assert!(spy.calls.is_empty());
    }
    let mut spy=Spy::default();
    assert!(execute(&mut spy,"query",&json("{}"),&[valid],Some(&policy(false)),Instant::now()).is_err());
    assert!(spy.calls.is_empty());
}
#[test]
fn dynamodb_recent_messages_preserves_descending_request_and_chronological_result() {
    let mut spy=Spy::default();
    spy.responses.push_back(Ok(json(r#"{"Items":[{"timestamp":9007199254740993,"ttl":9007199341140},{"timestamp":9007199254740992,"ttl":9007199341139}]}"#)));
    let result=run(&mut spy,"recent-messages",&json(r#"{"session_id":"s","limit":2}"#),None).unwrap();
    assert_eq!(spy.calls[0].0,Route::Dynamo);
    assert_eq!(spy.calls[0].1.get("ScanIndexForward"),Some(&Json::Bool(false)));
    assert_eq!(spy.calls[0].1.get("Limit"),Some(&Json::Int(2)));
    assert_eq!(result,json(r#"[{"timestamp":9007199254740992,"ttl":9007199341139},{"timestamp":9007199254740993,"ttl":9007199341140}]"#));
}
