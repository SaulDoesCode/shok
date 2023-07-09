const httpOutput = document.createElement('section')
httpOutput.classList.add('http-output')
httpOutput.addEventListener('pointerdown', e => {
    if (e.target.classList.contains('http-log')) {
        e.target.remove()
    } else if (e.target.parentNode.classList.contains('http-log')) {
        e.target.parentNode.remove()
    }
})

const logRes = (res, out, err) => httpOutput.innerHTML += `<section class="http-log">
<div class="status">${err == null ? '' : 'error: '} ${res.status} ${res.statusText}</div>
<div class="url">${res.url}</div>
<pre class="headers">${[...res.headers.entries()].map(([h, v]) => `${h}: ${v}`).join('\n')}</pre>
<div class="body">${JSON.stringify(out, null, 2)}</div></section>`

const defaultHeaders = {
    'Access-Control-Allow-Origin': '*'
}

const handleResponse = async res => {
    try {
        if (res instanceof Promise) res = await res
        const ct = res.headers.get('Content-Type')
        const out = await (
            ct.includes('application/json') ? res.json() :  ct.includes('text') ? res.text() : res.blob()
        )
        if (!res.ok) {
            logRes(res, out, res.statusText)
            return
        }
        return out
    } catch (err) {
        logRes(res, '', {err, statusText: res.statusText})
        throw new Error(res.statusText)
    }
} 
const mog = (method, body, ops = {}) => {
    const headers = ops.headers || {}
    delete ops.headers
    if (body == null) {
        delete headers['Content-Type']
    } else {
        if (typeof body == 'object' || Array.isArray(body)) {
            body = JSON.stringify(body)
            headers['Content-Type'] = 'application/json'
        } else if (body instanceof FormData) {
            headers['Content-Type'] = 'multipart/form-data'
        } else if (typeof body == 'string') {
            headers['Content-Type'] = 'application/json'
            body = `"${body}"`
        }
    }
    const m  = {method, body, headers: new Headers({...defaultHeaders, ...headers}), ...ops}
    return m
}
const fancify = fn => new Proxy(fn, {get: (f, url) => async (...args) => await f(url, ...args)})
const ppp = method => fancify(async (url, body, ops = {}) => await handleResponse(fetch(url, mog(method, body, ops))))
const gl = method => fancify(async (url, ops) => await handleResponse(fetch(url, mog(method, null, ops))))

document.body.append(httpOutput)

export default {
    post: ppp('POST'),
    put: ppp('PUT'),
    patch: ppp('PATCH'),
    get: gl('GET'),
    del: gl('DELETE')
}
