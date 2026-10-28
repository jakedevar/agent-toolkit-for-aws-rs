//! The declarative replacement of `cdk_test_env.py`: the CloudFormation
//! template is read and its safety properties are asserted. Nothing is
//! deployed.

use toolkit_core::{parse_json, Json};

const TEMPLATE: &str = include_str!(
    "../../../../../skills/specialized-skills/database-skills/amazon-neptune/scripts/neptune_test_env.cfn.json"
);

fn template() -> Json {
    parse_json(TEMPLATE).expect("the template is valid JSON")
}

fn resource<'a>(template: &'a Json, name: &str) -> &'a Json {
    template
        .get("Resources")
        .and_then(|r| r.get(name))
        .unwrap_or_else(|| panic!("resource {name}"))
}

fn property<'a>(resource: &'a Json, name: &str) -> &'a Json {
    resource
        .get("Properties")
        .and_then(|p| p.get(name))
        .unwrap_or_else(|| panic!("property {name}"))
}

fn resources_of_type<'a>(template: &'a Json, kind: &str) -> Vec<(&'a str, &'a Json)> {
    let Some(Json::Object(all)) = template.get("Resources") else {
        panic!("Resources");
    };
    all.iter()
        .filter(|(_, r)| r.get("Type").and_then(Json::as_str) == Some(kind))
        .map(|(name, r)| (name.as_str(), r))
        .collect()
}

#[test]
fn the_cluster_models_the_secure_patterns_of_the_stack() {
    let t = template();
    let cluster = resource(&t, "NeptuneCluster");
    assert_eq!(property(cluster, "StorageEncrypted"), &Json::Bool(true));
    assert_eq!(property(cluster, "IamAuthEnabled"), &Json::Bool(true));
    let scaling = property(cluster, "ServerlessScalingConfiguration");
    assert_eq!(scaling.get("MinCapacity"), Some(&Json::Int(1)));
    assert_eq!(scaling.get("MaxCapacity"), Some(&Json::Int(4)));
    assert_eq!(
        resource(&t, "NeptuneInstance")
            .get("Properties")
            .and_then(|p| p.get("DBInstanceClass"))
            .and_then(Json::as_str),
        Some("db.serverless")
    );
    let tags = property(cluster, "Tags").to_compact_string();
    assert!(tags.contains("\"created_by\"") && tags.contains("\"neptune-skill\""));
    assert!(tags.contains("\"generation_model\"") && tags.contains("GenerationModel"));
}

#[test]
fn the_database_listens_only_inside_the_vpc_on_the_neptune_port() {
    let t = template();
    let group = resource(&t, "NeptuneSecurityGroup");
    let ingress = property(group, "SecurityGroupIngress").to_compact_string();
    assert_eq!(
        ingress.matches("FromPort").count(),
        1,
        "one ingress rule: {ingress}"
    );
    assert!(ingress.contains("\"FromPort\":8182") && ingress.contains("\"ToPort\":8182"));
    assert!(ingress.contains("\"CidrIp\":{\"Ref\":\"VpcCidr\"}"));
    assert!(!ingress.contains("0.0.0.0/0") && !ingress.contains("::/0"));
    // The VPC range is itself a private range.
    let pattern = t
        .get("Parameters")
        .and_then(|p| p.get("VpcCidr"))
        .and_then(|c| c.get("AllowedPattern"))
        .and_then(Json::as_str)
        .expect("pattern");
    assert!(pattern.contains("(10|172|192)"));
    // The cluster lives in the private subnets only.
    let subnets = property(resource(&t, "NeptuneSubnetGroup"), "SubnetIds").to_compact_string();
    assert!(subnets.contains("PrivateSubnet1") && subnets.contains("PrivateSubnet2"));
    assert!(!subnets.contains("PublicSubnet"));
    for name in ["PrivateSubnet1", "PrivateSubnet2"] {
        assert_eq!(
            property(resource(&t, name), "MapPublicIpOnLaunch"),
            &Json::Bool(false)
        );
    }
    assert_eq!(
        property(resource(&t, "NeptuneCluster"), "VpcSecurityGroupIds").to_compact_string(),
        "[{\"Ref\":\"NeptuneSecurityGroup\"}]"
    );
}

#[test]
fn the_network_matches_the_stack() {
    let t = template();
    assert_eq!(
        resources_of_type(&t, "AWS::EC2::Subnet").len(),
        4,
        "two zones, private and public"
    );
    assert_eq!(
        resources_of_type(&t, "AWS::EC2::NatGateway").len(),
        1,
        "nat_gateways=1"
    );
    assert_eq!(resources_of_type(&t, "AWS::EC2::InternetGateway").len(), 1);
    let route = property(resource(&t, "PrivateDefaultRoute"), "NatGatewayId").to_compact_string();
    assert!(route.contains("NatGateway"));
    assert!(property(resource(&t, "PublicDefaultRoute"), "GatewayId")
        .to_compact_string()
        .contains("InternetGateway"));
}

#[test]
fn the_seed_role_is_scoped_to_the_cluster() {
    let t = template();
    let role = resource(&t, "SeedLambdaRole");
    let assume = property(role, "AssumeRolePolicyDocument").to_compact_string();
    assert!(assume.contains("lambda.amazonaws.com"));
    assert!(property(role, "ManagedPolicyArns")
        .to_compact_string()
        .contains("AWSLambdaVPCAccessExecutionRole"));
    let policies = property(role, "Policies").to_compact_string();
    assert!(
        policies.contains("neptune-db:") && policies.contains("NeptuneCluster.ClusterResourceId")
    );
    assert!(
        !policies.contains("\"Resource\":\"*\""),
        "no wildcard resource: {policies}"
    );
    // The data-plane grant lists exactly the actions the seed and validation
    // flows use (#4905); no `neptune-db:*`.
    let statement = role
        .get("Properties")
        .and_then(|p| p.get("Policies"))
        .and_then(|p| match p {
            Json::Array(items) => items.first(),
            _ => None,
        })
        .and_then(|p| p.get("PolicyDocument"))
        .and_then(|d| d.get("Statement"))
        .and_then(|s| match s {
            Json::Array(items) => items.first(),
            _ => None,
        })
        .expect("seed role statement");
    let Some(Json::Array(actions)) = statement.get("Action") else {
        panic!("Action must be an explicit list: {policies}");
    };
    let actions: Vec<&str> = actions.iter().filter_map(Json::as_str).collect();
    assert_eq!(actions, SEED_ROLE_ACTIONS);
}

/// The neptune-db actions of the seed role, in template order.
const SEED_ROLE_ACTIONS: [&str; 6] = [
    "neptune-db:connect",
    "neptune-db:ReadDataViaQuery",
    "neptune-db:WriteDataViaQuery",
    "neptune-db:DeleteDataViaQuery",
    "neptune-db:GetQueryStatus",
    "neptune-db:GetEngineStatus",
];

/// Every `Action` / `NotAction` value anywhere under `node` that contains an
/// IAM wildcard (`*` or `?`): the bare `*`, a service wildcard such as
/// `svc:*` and a partial one such as `svc:Get*` alike. A `NotAction` is
/// reported whatever it holds: it grants everything it does not list.
fn wildcard_actions(node: &Json) -> Vec<String> {
    fn walk(node: &Json, found: &mut Vec<String>) {
        match node {
            Json::Object(members) => {
                for (key, value) in members {
                    if key == "NotAction" {
                        found.push(format!("NotAction {}", value.to_compact_string()));
                    } else if key == "Action" {
                        let listed: Vec<&Json> = match value {
                            Json::Array(items) => items.iter().collect(),
                            other => vec![other],
                        };
                        for action in listed {
                            match action.as_str() {
                                Some(text) if !text.contains(['*', '?']) => {}
                                _ => found.push(action.to_compact_string()),
                            }
                        }
                    }
                    walk(value, found);
                }
            }
            Json::Array(items) => items.iter().for_each(|item| walk(item, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(node, &mut found);
    found
}

#[test]
fn no_policy_in_the_template_grants_a_wildcard_action() {
    let t = template();
    assert_eq!(wildcard_actions(&t), Vec::<String>::new());
}

#[test]
fn the_wildcard_check_rejects_every_wildcard_shape() {
    for action in [
        r#""*""#,
        r#""neptune-db:*""#,
        r#""neptune-db:Read*""#,
        r#""neptune-db:Get?ueryStatus""#,
        r#"["neptune-db:connect", "iam:*"]"#,
        r#"{"Fn::Sub": "neptune-db:connect"}"#,
    ] {
        let document = parse_json(&format!(
            r#"{{"Resources": {{"R": {{"Properties": {{"Policies": [{{"PolicyDocument": {{"Statement": [{{"Effect": "Allow", "Action": {action}, "Resource": "arn"}}]}}}}]}}}}}}}}"#
        ))
        .expect("fixture");
        assert_eq!(wildcard_actions(&document).len(), 1, "{action}");
    }
    let not_action = parse_json(
        r#"{"Statement": [{"Effect": "Allow", "NotAction": "neptune-db:connect", "Resource": "arn"}]}"#,
    )
    .expect("fixture");
    assert_eq!(wildcard_actions(&not_action).len(), 1);
    let explicit =
        parse_json(r#"{"Action": ["neptune-db:connect", "sts:AssumeRole"]}"#).expect("fixture");
    assert!(wildcard_actions(&explicit).is_empty());
}

/// The deletion-safe defaults (#4906): the cluster is snapshotted on deletion
/// and on replacement and is deletion-protected unless the stack is launched
/// with the explicit, non-default `DeletionProtection=false`; only then may
/// the instance be deleted with the stack.
#[test]
fn the_cluster_is_deletion_safe_by_default() {
    let t = template();
    let parameter = t
        .get("Parameters")
        .and_then(|p| p.get("DeletionProtection"))
        .expect("DeletionProtection parameter");
    assert_eq!(
        parameter.get("Default").and_then(Json::as_str),
        Some("true")
    );
    assert_eq!(
        parameter.get("AllowedValues").map(Json::to_compact_string),
        Some("[\"true\",\"false\"]".to_string())
    );
    let cluster = resource(&t, "NeptuneCluster");
    assert_eq!(
        property(cluster, "DeletionProtection").to_compact_string(),
        "{\"Ref\":\"DeletionProtection\"}"
    );
    for policy in ["DeletionPolicy", "UpdateReplacePolicy"] {
        assert_eq!(
            cluster.get(policy).and_then(Json::as_str),
            Some("Snapshot"),
            "cluster {policy}"
        );
    }
    // The conditional instance policies need the language extensions.
    assert_eq!(
        t.get("Transform").and_then(Json::as_str),
        Some("AWS::LanguageExtensions")
    );
    assert_eq!(
        t.get("Conditions")
            .and_then(|c| c.get("TeardownAllowed"))
            .map(Json::to_compact_string),
        Some("{\"Fn::Equals\":[{\"Ref\":\"DeletionProtection\"},\"false\"]}".to_string())
    );
    let instance = resource(&t, "NeptuneInstance");
    for policy in ["DeletionPolicy", "UpdateReplacePolicy"] {
        assert_eq!(
            instance.get(policy).map(Json::to_compact_string),
            Some("{\"Fn::If\":[\"TeardownAllowed\",\"Delete\",\"Retain\"]}".to_string()),
            "instance {policy}"
        );
    }
    // No resource anywhere deletes unconditionally.
    let Some(Json::Object(all)) = t.get("Resources") else {
        panic!("Resources");
    };
    for (name, resource) in all {
        for policy in ["DeletionPolicy", "UpdateReplacePolicy"] {
            assert_ne!(
                resource.get(policy).and_then(Json::as_str),
                Some("Delete"),
                "{name} {policy}"
            );
        }
    }
    let description = t.get("Description").and_then(Json::as_str).expect("text");
    assert!(
        description.contains("DeletionProtection=false"),
        "{description}"
    );
    assert!(!description.contains("THROWAWAY"), "{description}");
}

#[test]
fn the_outputs_are_the_ones_the_eval_scripts_read() {
    let t = template();
    let Some(Json::Object(outputs)) = t.get("Outputs") else {
        panic!("Outputs")
    };
    let names: Vec<&str> = outputs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "NeptuneEndpoint",
            "NeptunePort",
            "VpcId",
            "PrivateSubnetId",
            "NeptuneSecurityGroupId"
        ]
    );
    assert_eq!(
        t.get("Outputs")
            .and_then(|o| o.get("NeptuneEndpoint"))
            .and_then(|e| e.get("Export"))
            .and_then(|e| e.get("Name"))
            .and_then(Json::as_str),
        Some("NeptuneSkillTestEndpoint")
    );
}

#[test]
fn no_resource_creates_an_internet_facing_database_path() {
    let t = template();
    let text = t.to_compact_string();
    assert!(!text.contains("PubliclyAccessible"));
    assert!(!text.contains("\"CidrIp\":\"0.0.0.0/0\",\"Description\":\"Neptune"));
    // The only 0.0.0.0/0 uses are the two default routes and the egress rule.
    assert_eq!(text.matches("0.0.0.0/0").count(), 3);
}
