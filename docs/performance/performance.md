# Orion Proxy Performance Benchmarking

## Results

### HTTP 

#### Requests per second 
<img src="../pics/performance/baseline/wrk-reqsec.png" alt="requests per second" style="width:800px;"/>

#### Latency in microseconds
<img src="../pics/performance/baseline/wrk-latency.png" alt="latency in microseconds" style="width:800px;"/>

### HTTPS
#### Requests per second 
<img src="../pics/performance/tls/wrk-reqsec.png" alt="requests per second" style="width:800px;"/>

#### Latency in microseconds
<img src="../pics/performance/tls/wrk-latency.png" alt="requests per second" style="width:800px;"/>

### TCP
#### Requests per second 
<img src="../pics/performance/tcp/wrk-reqsec.png" alt="requests per second" style="width:800px;"/>

#### Latency in microseconds
<img src="../pics/performance/tcp/wrk-latency.png" alt="requests per second" style="width:800px;"/>

## Test Methodology and Testbed
<img src="../pics/performance/testbed_conf.png" alt="testbed" style="width:800px;"/>


* Requests generator (wrk):
  - 2048 and 16384 connections tests
  - 45s duration per test
  - 36 threads/18 cores for the application on NUMA0
  - 6 cores assigned to IRQ handling on NUMA0
  - Measures req/s, throughput, latency (min, max, mean, 99.9th, 99.999th)
* Upstream Cluster (Nginx):
  - In-memory direct response of 12B size
  - 17 cores assigned to the application on NUMA1
  - 7 cores assigned to IRQ handling on NUMA1
  - 2.6M RPS à hard-limit of the server
  - Measures #connections established upstream

* DUT (Proxy):
  - Comparison Envoy, Ng-proxy, and Nginx
  - Scaling tested with 1, 2, 4, 8, 16, 24, 32 cores assigned to the proxy
  - TCP termination toward single upstream cluster

* DUT (Tunings):
  - IRQ affinity
  - 8 cores reserved for the IRQ affinity.
  - 24 cores allocated for proxy (per NUMA node).
* All interfaces are 25Gbps Mellanox and Intel NICs
