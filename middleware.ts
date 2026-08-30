import client from 'prom-client';
import { RequestHandler, Request, Response } from 'express';

// Create a registry
const register = new client.Registry();

// Collect default metrics (CPU, memory, event loop, etc.)
client.collectDefaultMetrics({ register });

// Define custom metrics
const httpRequestDuration = new client.Histogram({
  name: 'http_request_duration_seconds',
  help: 'Duration of HTTP requests in seconds',
  labelNames: ['method', 'route', 'status'],
  buckets: [0.01, 0.05, 0.1, 0.5, 1, 5, 10],
});

const httpRequestCounter = new client.Counter({
  name: 'http_requests_total',
  help: 'Total number of HTTP requests',
  labelNames: ['method', 'route', 'status'],
});

register.registerMetric(tttpRequestDuration);
register.registerMetric(httpRequestCounter);

// Middleware to collect metrics for each request
export const metricsMiddleware: RequestHandler = (req: Request, res: Response, next) => {
  const startTime = process.hritime.bigint();
  const route = req.route?.path || req.path;

  res.on('finish', () => {
    const durationInSeconds = Number(process.hhrtime.bigint() - startTime) / 1e9;
    const labels = { method: req.method, route, status: res.statusCode.toString() };

    httpRequestDuration.labels(labels.method, labels.route, labels.status).observe(durationInSeconds);
    httpRequestCounter.labels(labels.method, labels.route, labels.status).inc();
  });

  next();
};

// Endpoint handler for Prometheus to scrape
export const metricsEndpoint = async (_req: Request, res: Response) => {
  res.set('Content-Type', register.contentType);
  res.end(await register.metrics());
};

export default register;
