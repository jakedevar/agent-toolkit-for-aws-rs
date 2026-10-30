use super::*;
use crate::docgraph::live_memory::tests::{json,policy};
use toolkit_aws::testing::{FakeChunk,FakeStreamingTransport,FakeSigner};
use toolkit_aws::transport::{Method,FRAMING_HEADERS};
fn query() -> Json { memory::query("g-abc123def4","RETURN $x",json(r#"{"x":[9007199254740993,true,null,"'雪",{"k":0.5}]}"#)).unwrap() }
fn encoded() -> HttpRequest { encode(Route::Analytics,&query(),"us-east-1",Some(&policy(false)),Duration::from_secs(60)).unwrap() }
#[test]
fn analytics_uses_modeled_query_wire_fields_endpoint_parameters_and_timeout() {
    let request=encoded();
    assert_eq!(request.method,Method::Post);
    assert_eq!(request.url,"https://g-abc123def4.us-east-1.neptune-graph.amazonaws.com:443/queries");
    assert_eq!(request.headers.get("graphidentifier"),Some("g-abc123def4"));
    let body=json(std::str::from_utf8(&request.body).unwrap());
    assert_eq!(body.get("query"),query().get("queryString"));
    assert_eq!(body.get("language"),Some(&Json::string("OPEN_CYPHER")));
    assert_eq!(body.get("parameters"),query().get("parameters"));
    assert_eq!(body.get("queryTimeoutMilliseconds"),Some(&Json::Int(55000)));
    assert!(FRAMING_HEADERS.iter().all(|h|request.headers.get(h).is_none()));
    let short=encode(Route::Analytics,&query(),"us-east-1",Some(&policy(false)),Duration::from_secs(4)).unwrap();
    assert_eq!(json(std::str::from_utf8(&short.body).unwrap()).get("queryTimeoutMilliseconds"),Some(&Json::Int(3000)));
    assert!(encode(Route::Analytics,&query(),"us-east-1",Some(&policy(false)),Duration::from_millis(1000)).is_err());
}
#[test]
fn secure_creation_is_a_distinct_modeled_control_plane_request() {
    let planned=memory::create_graph("example",32,1536,"model",true);
    let request=encode(Route::Create,&planned,"us-east-1",None,Duration::from_secs(60)).unwrap();
    assert_eq!(request.url,"https://neptune-graph.us-east-1.amazonaws.com/graphs");
    assert_eq!(json(std::str::from_utf8(&request.body).unwrap()),planned);
    assert_eq!(normalize(Route::Create,br#"{"id":"g-abc123def4","endpoint":"ignored","status":"CREATING"}"#).unwrap(),json(r#"{"id":"g-abc123def4"}"#));
    for bad in [br#"{"id":null}"#.as_slice(),br#"{"id":"bad"}"#,br#"[]"#] { assert!(normalize(Route::Create,bad).is_err()); }
}
#[test]
fn dynamodb_typed_encoding_and_decoding_preserve_integers_and_reject_unsupported_types() {
    let plan=memory::plan("store-message",&json(r#"{"session_id":"s","role":"user","content":"hello","now_ms":9007199254740993}"#)).unwrap();
    let before=plan.clone();
    let request=encode(Route::Dynamo,&plan[0],"us-east-1",None,Duration::from_secs(60)).unwrap();
    assert_eq!(plan,before);
    assert_eq!(request.url,"https://dynamodb.us-east-1.amazonaws.com/");
    assert_eq!(request.headers.get("x-amz-target"),Some("DynamoDB_20120810.PutItem"));
    let body=json(std::str::from_utf8(&request.body).unwrap());
    assert_eq!(body.get("TableName"),Some(&Json::string("agent-short-term-memory")));
    assert_eq!(body.get("Item").unwrap().get("timestamp"),Some(&json(r#"{"N":"9007199254740993"}"#)));
    assert_eq!(body.get("Item").unwrap().get("ttl"),Some(&json(r#"{"N":"9007199341140"}"#)));
    assert_eq!(normalize(Route::Dynamo,r#"{"Items":[{"s":{"S":"雪"},"n":{"N":"9007199254740993"},"b":{"BOOL":false},"z":{"NULL":true},"l":{"L":[{"N":"-2"}]},"m":{"M":{"x":{"S":"y"}}}}]}"#.as_bytes()).unwrap(),json(r#"{"Items":[{"s":"雪","n":9007199254740993,"b":false,"z":null,"l":[-2],"m":{"x":"y"}}]}"#));
    for attr in [r#"{"N":"NaN"}"#,r#"{"N":"9223372036854775808"}"#,r#"{"N":"1.5"}"#,r#"{"B":"AA=="}"#,r#"{"SS":["a"]}"#,r#"{"S":"x","N":"1"}"#,r#"{"NULL":false}"#] {
        assert!(normalize(Route::Dynamo,format!(r#"{{"Items":[{{"x":{attr}}}]}}"#).as_bytes()).is_err(),"{attr}");
    }
    let bad=memory::plan("store-message",&json(r#"{"session_id":"s","role":"user","content":1.5,"now_ms":1}"#)).unwrap();
    assert!(encode(Route::Dynamo,&bad[0],"us-east-1",None,Duration::from_secs(60)).is_err());
}
#[test]
fn malformed_payloads_and_failed_streams_have_one_attempt_and_fixed_errors() {
    for (status,body) in [(200,br#"{"results":[]}"#.to_vec()),(200,br#"{"results":null}"#.to_vec()),(200,br#"{"results":{}}"#.to_vec()),(200,br#"{"results":["#.to_vec()),(200,br#"[]"#.to_vec()),(503,b"secret-response".to_vec()),(403,b"secret-response".to_vec())] {
        let transport=FakeStreamingTransport::new().respond(status,&[],vec![FakeChunk::Data(body.clone())]);
        let signer=FakeSigner::new();
        let result=exchange(&transport,&signer,Route::Analytics,encoded(),"us-east-1",Instant::now()+Duration::from_secs(5));
        assert_eq!(result.is_ok(),status==200 && body==br#"{"results":[]}"#);
        if let Err(e)=result {assert!(!e.contains("secret-response"));assert!(e.contains("unknown outcome"),"{e}");}
        assert_eq!(transport.requests().len(),1);
        assert_eq!(signer.signed(),1);
        assert_eq!(transport.requests()[0].headers.get("authorization"),Some("FAKE-SIGNED us-east-1/neptune-graph"));
    }
    for chunk in [FakeChunk::IdleTimeout,FakeChunk::Fail(toolkit_aws::AwsError::connect("secret"))] {
        let transport=FakeStreamingTransport::new().respond(200,&[],vec![chunk]);
        assert!(exchange(&transport,&FakeSigner::new(),Route::Analytics,encoded(),"us-east-1",Instant::now()+Duration::from_secs(5)).unwrap_err().contains("unknown outcome"));
        assert_eq!(transport.requests().len(),1);
    }
    let transport=FakeStreamingTransport::new();
    assert!(exchange(&transport,&toolkit_aws::NoSigner,Route::Analytics,encoded(),"us-east-1",Instant::now()+Duration::from_secs(5)).is_err());
    assert!(transport.requests().is_empty());
}
#[test]
fn http_response_limit_boundary_and_expired_deadline_are_enforced() {
    for extra in [0,1] {
        let mut bytes=br#"{"results":[]}"#.to_vec();
        bytes.resize(16*1024*1024+extra,b' ');
        let transport=FakeStreamingTransport::new().respond(200,&[],vec![FakeChunk::Data(bytes)]);
        let result=exchange(&transport,&FakeSigner::new(),Route::Analytics,encoded(),"us-east-1",Instant::now()+Duration::from_secs(20));
        assert_eq!(result.is_ok(),extra==0);
    }
    let transport=FakeStreamingTransport::new();
    let signer=FakeSigner::new();
    assert!(exchange(&transport,&signer,Route::Analytics,encoded(),"us-east-1",Instant::now()).is_err());
    assert!(transport.requests().is_empty());
    assert_eq!(signer.signed(),0);
}
