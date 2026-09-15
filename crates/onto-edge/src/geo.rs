// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! 地理位置路由 - 基于 GPS 坐标自动匹配最佳节点

use super::GeoLocation;
use serde::{Deserialize, Serialize};

/// 区域节点信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionNode {
    pub node_id: String,
    pub region: String,
    pub center_lat: f64,
    pub center_lng: f64,
    pub hub_url: String,
}

/// 地理路由器
pub struct GeoRouter {
    nodes: Vec<RegionNode>,
}

impl GeoRouter {
    pub fn new() -> Self {
        let mut router = Self { nodes: Vec::new() };
        // 预设中国主要区域节点
        router.add_default_nodes();
        router
    }

    /// 添加区域节点
    pub fn add_node(&mut self, node: RegionNode) {
        self.nodes.push(node);
    }

    /// 根据 GPS 坐标找最近的节点
    pub fn find_nearest(&self, location: &GeoLocation) -> Option<&RegionNode> {
        self.nodes.iter().min_by(|a, b| {
            let dist_a = haversine_distance(
                location.latitude,
                location.longitude,
                a.center_lat,
                a.center_lng,
            );
            let dist_b = haversine_distance(
                location.latitude,
                location.longitude,
                b.center_lat,
                b.center_lng,
            );
            dist_a.partial_cmp(&dist_b).unwrap()
        })
    }

    /// 根据区域名找节点
    pub fn find_by_region(&self, region: &str) -> Option<&RegionNode> {
        self.nodes.iter().find(|n| n.region == region)
    }

    /// 获取所有节点
    pub fn get_nodes(&self) -> &[RegionNode] {
        &self.nodes
    }

    /// 添加中国默认区域节点
    fn add_default_nodes(&mut self) {
        let defaults = vec![
            ("beijing", "华北", 39.9042, 116.4074),
            ("shanghai", "华东", 31.2304, 121.4737),
            ("zhengzhou", "华中", 34.7466, 113.6253),
            ("guangzhou", "华南", 23.1291, 113.2644),
            ("chengdu", "西南", 30.5728, 104.0668),
            ("xian", "西北", 34.3416, 108.9398),
            ("shenyang", "东北", 41.8057, 123.4315),
        ];

        for (node_id, region, lat, lng) in defaults {
            self.nodes.push(RegionNode {
                node_id: node_id.to_string(),
                region: region.to_string(),
                center_lat: lat,
                center_lng: lng,
                hub_url: format!("http://{}.valuehub.io:7915", node_id),
            });
        }
    }
}

/// Haversine 距离计算（米）
fn haversine_distance(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let r = 6371000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlng = (lng2 - lng1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlng / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    r * c
}
