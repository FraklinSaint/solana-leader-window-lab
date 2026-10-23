"""slwlab.independence: TPU-IP grouping (same rule as select.rs shared_tpu_ips)."""

from slwlab import independence as ind


def test_groups_and_units():
    nodes = [{"tpuQuic": "198.13.140.105:12341"}, {"tpuQuic": "198.13.140.105:12349"},
             {"tpuQuic": "198.13.140.105:12000"}, {"tpuQuic": "208.91.105.55:5010"},
             {"tpuQuic": "208.91.105.55:5011"}, {"tpuQuic": "5.199.172.176:12002"}, {"tpuQuic": None}]
    picked = [{"identity": "H2", "tpu_quic": "198.13.140.105:12349"},
              {"identity": "H1", "tpu_quic": "198.13.140.105:12341"},
              {"identity": "B1", "tpu_quic": "208.91.105.55:5010"},
              {"identity": "A1", "tpu_quic": "5.199.172.176:12002"},
              {"identity": "N1", "tpu_quic": None}]
    assert ind.shared_tpu_ips(nodes, picked) == [
        {"ip": "198.13.140.105", "gossip_nodes_on_ip": 3, "picked_on_ip": ["H1", "H2"]},
        {"ip": "208.91.105.55", "gossip_nodes_on_ip": 2, "picked_on_ip": ["B1"]},
    ]
    u = ind.units(picked)
    assert u["H1"] == u["H2"] != u["B1"]
    assert len(set(u.values())) == 4  # H1+H2 one unit, B1, A1, N1
    assert ind.tpu_ip("[2001:db8::1]:8009") == "2001:db8::1"
    lines = [{"tx": {"identity": "H1", "tpu_quic": "198.13.140.105:12341"}},
             {"tx": {"identity": "H2", "tpu_quic": "198.13.140.105:12349"}}]
    assert len(set(ind.units_from_manifest(lines).values())) == 1
