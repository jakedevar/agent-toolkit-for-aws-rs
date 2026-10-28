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
    // Throwaway cluster: deletion protection off and delete on removal, as in
    // the CDK stack; the description tells production to turn it on.
    assert_eq!(property(cluster, "DeletionProtection"), &Json::Bool(false));
    assert_eq!(
        cluster.get("DeletionPolicy").and_then(Json::as_str),
        Some("Delete")
    );
    let scaling = property(cluster, "ServerlessScalingConfiguration");
    assert_eq!(scaling.get("MinCapacity"), Some(&Json::Int(1)));
    assert_eq!(scaling.get("MaxCapacity"), Some(&Json::Int(4)));
    assert!(t
        .get("Description")
        .and_then(Json::as_str)
        .is_some_and(|d| d.contains("DeletionProtection to true")));
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
    for (name, other) in resources_of_type(&t, "AWS::IAM::Role") {
        assert!(
            !other.to_compact_string().contains("\"Action\":\"*\""),
            "{name}"
        );
    }
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
