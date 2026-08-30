# Kubernetes Deployment

## Basic Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: ontodb
spec:
  replicas: 1
  selector:
    matchLabels:
      app: ontodb
  template:
    metadata:
      labels:
        app: ontodb
    spec:
      containers:
        - name: ontodb
          image: ontodb/ontodb:latest
          args: ["--data-dir", "/data", "--http", "0.0.0.0:7912"]
          ports:
            - containerPort: 7912
          volumeMounts:
            - name: data
              mountPath: /data
          livenessProbe:
            httpGet:
              path: /api/health
              port: 7912
            initialDelaySeconds: 5
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /api/health
              port: 7912
            initialDelaySeconds: 3
            periodSeconds: 5
          resources:
            requests:
              memory: "256Mi"
              cpu: "250m"
            limits:
              memory: "1Gi"
              cpu: "1000m"
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: ontodb-data
---
apiVersion: v1
kind: Service
metadata:
  name: ontodb
spec:
  selector:
    app: ontodb
  ports:
    - port: 7912
      targetPort: 7912
  type: ClusterIP
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: ontodb-data
spec:
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: 10Gi
```

## With Authentication

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: ontodb-auth
type: Opaque
stringData:
  api-key: "your-secret-key"
---
# Add to Deployment spec:
env:
  - name: AUTH_ENABLED
    value: "true"
  - name: AUTH_API_KEY
    valueFrom:
      secretKeyRef:
        name: ontodb-auth
        key: api-key
```

## Scaling

OntoDB supports horizontal scaling with Raft consensus (Enterprise edition):

```yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: ontodb
spec:
  replicas: 3
  # ... Raft configuration via args
```
